// pyramid/evidence_answering.rs — Evidence-Weighted Answering (Steps 3.1–3.2)
//
// Two-phase approach to building upper-layer pyramid nodes:
//   1. pre_map_layer()   — Horizontal pre-mapping: one LLM call maps ALL questions
//                          to candidate evidence nodes from the layer below.
//   2. answer_questions() — Vertical answering: parallel per-question LLM calls that
//                          evaluate candidates, produce KEEP/DISCONNECT/MISSING verdicts,
//                          and synthesize answers into new pyramid nodes.
//
// This replaces the old clustering/synthesis approach with question-driven evidence
// answering. Each upper-layer node is the answer to a specific question, grounded
// in weighted evidence links to lower-layer nodes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use std::path::PathBuf;

use rusqlite;

use super::db;
use super::llm::{self, AuditContext, LlmConfig};
use super::question_decomposition::render_prompt_template;
use super::step_context::make_step_ctx_from_llm_config;
use super::types::{
    AnswerBatchResult, AnsweredNode, CandidateMap, EvidenceLink, EvidenceSet, EvidenceVerdict,
    FailedQuestion, LayerQuestion, ProvenanceKind, PyramidNode,
};
use super::OperationalConfig;

const PRE_MAP_SAVE_MAX_RETRIES: u32 = 3;
const PRE_MAP_SAVE_BACKOFF_MS: [u64; 3] = [100, 500, 2000];

fn is_sqlite_busy_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("database is locked")
        || message.contains("database busy")
        || message.contains("database is busy")
}

fn valid_candidate_links_for_batch(
    mappings: &HashMap<String, Vec<String>>,
    valid_ids: &HashSet<&str>,
) -> HashMap<String, Vec<String>> {
    let mut filtered = HashMap::new();
    for (question_id, candidates) in mappings {
        let mut valid: Vec<String> = candidates
            .iter()
            .filter_map(|id| {
                let trimmed = id.trim();
                if trimmed.is_empty() || !valid_ids.contains(trimmed) {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
            .collect();
        valid.sort();
        valid.dedup();
        if !valid.is_empty() {
            filtered.insert(question_id.clone(), valid);
        }
    }
    filtered
}

async fn persist_pre_map_candidate_links(
    audit_ctx: Option<&AuditContext>,
    layer: i64,
    batch_idx: usize,
    audit_id: Option<i64>,
    mappings: HashMap<String, Vec<String>>,
) -> Result<usize> {
    if mappings.is_empty() {
        return Ok(0);
    }
    let Some(audit_ctx) = audit_ctx else {
        return Ok(0);
    };

    let conn = Arc::clone(&audit_ctx.conn);
    let slug = audit_ctx.slug.clone();
    let build_id = audit_ctx.build_id.clone();

    tokio::task::spawn_blocking(move || {
        let c = conn.blocking_lock();
        for attempt in 0..=PRE_MAP_SAVE_MAX_RETRIES {
            let tx_result = (|| -> Result<usize> {
                c.execute_batch("BEGIN IMMEDIATE")?;
                let inner = db::save_candidate_link_batch(
                    &c,
                    &slug,
                    &build_id,
                    layer,
                    batch_idx as i64,
                    "pre_map",
                    audit_id,
                    &mappings,
                );
                match inner {
                    Ok(saved) => {
                        if let Err(e) = c.execute_batch("COMMIT") {
                            let _ = c.execute_batch("ROLLBACK");
                            Err(e.into())
                        } else {
                            Ok(saved)
                        }
                    }
                    Err(e) => {
                        let _ = c.execute_batch("ROLLBACK");
                        Err(e)
                    }
                }
            })();

            match tx_result {
                Ok(saved) => return Ok(saved),
                Err(e) if is_sqlite_busy_error(&e) && attempt < PRE_MAP_SAVE_MAX_RETRIES => {
                    warn!(
                        slug = %slug,
                        batch = batch_idx,
                        attempt,
                        backoff_ms = PRE_MAP_SAVE_BACKOFF_MS[attempt as usize],
                        "pre-map candidate-link save hit database-locked, retrying"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(
                        PRE_MAP_SAVE_BACKOFF_MS[attempt as usize],
                    ));
                }
                Err(e) => return Err(e),
            }
        }

        Ok(0)
    })
    .await
    .map_err(|e| anyhow!("pre-map candidate-link save panicked: {e}"))?
}

/// Check if an L0 node ID is a targeted re-examination.
/// Canonical L0 nodes use patterns like C-L0-001, D-L0-042, or short sequential IDs.
/// Targeted evidence nodes historically used L0-{uuid}; current gap filling
/// allocates transaction-scoped L0-TNNN IDs.
fn is_targeted_l0_id(id: &str) -> bool {
    if let Some(suffix) = id.strip_prefix("L0-T") {
        return !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit());
    }

    // Targeted legacy: "L0-" followed by a UUID (36 chars with hyphens,
    // e.g., L0-491a10ef-4b59-...).
    if let Some(suffix) = id.strip_prefix("L0-") {
        suffix.len() >= 36 && suffix.chars().nth(8) == Some('-')
    } else {
        false
    }
}

// ── L0 Summary Helper ────────────────────────────────────────────────────────

/// Build a summary string from L0 nodes for use in synthesis prompt generation.
///
/// Concatenates each node's headline + distilled text (truncated to ~200 chars
/// per node). Total budget: ~100K chars. If it exceeds that, truncates from the end.
pub fn build_l0_summary(nodes: &[PyramidNode], ops: &OperationalConfig) -> String {
    let budget = ops.tier2.l0_summary_budget;
    let mut summary = String::new();
    for node in nodes {
        let distilled_trunc: String = node.distilled.chars().take(200).collect();
        let entry = format!("- {}: {}\n", node.headline, distilled_trunc);
        if summary.len() + entry.len() > budget {
            summary.push_str("... (truncated)\n");
            break;
        }
        summary.push_str(&entry);
    }
    summary
}

// ── Step 3.1: Horizontal Pre-Mapping ─────────────────────────────────────────

/// Map all questions for a layer to candidate evidence nodes from the layer below.
///
/// One LLM call reads ALL questions + ALL node headlines/distilled from the lower
/// layer. Returns a CandidateMap (question_id → [candidate_node_ids]).
///
/// Intentionally OVER-INCLUDES candidates — better a false positive than a miss.
/// The answering step (3.2) will prune irrelevant candidates via verdicts.
///
/// Uses mercury-2 (fast model) since this is classification, not synthesis.
pub async fn pre_map_layer(
    questions: &[LayerQuestion],
    lower_layer_nodes: &[PyramidNode],
    llm_config: &LlmConfig,
    ops: &OperationalConfig,
    audience: Option<&str>,
    chains_dir: Option<&PathBuf>,
    source_content_type: Option<&str>,
    _evidence_sets: Option<&[EvidenceSet]>, // loaded by caller, None for single-pass
    audit: Option<&AuditContext>,
) -> Result<CandidateMap> {
    if questions.is_empty() {
        return Ok(CandidateMap {
            mappings: HashMap::new(),
        });
    }
    if lower_layer_nodes.is_empty() {
        // No evidence to map — return empty candidates for each question
        let mappings = questions
            .iter()
            .map(|q| (q.question_id.clone(), Vec::new()))
            .collect();
        return Ok(CandidateMap { mappings });
    }

    // ── Build question listing ──────────────────────────────────────────
    let questions_text = questions
        .iter()
        .map(|q| {
            format!(
                "  - id: \"{}\"\n    question: \"{}\"\n    about: \"{}\"\n    creates: \"{}\"",
                q.question_id, q.question_text, q.about, q.creates
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // ── Build node payloads as Value items for batching ────────────────
    let node_payloads: Vec<serde_json::Value> = lower_layer_nodes
        .iter()
        .map(|n| {
            serde_json::json!({
                "id": n.id,
                "headline": n.headline.chars().take(200).collect::<String>(),
                "distilled": n.distilled.chars().take(300).collect::<String>(),
                "topics": n.topics.iter().map(|t| {
                    let summary = t.extra.get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    serde_json::json!({
                        "name": &t.name,
                        "summary": summary,
                        "current": t.current.chars().take(240).collect::<String>(),
                        "entities": &t.entities,
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();

    // Token-aware batching: pack nodes into batches that fit within budget,
    // adaptively dehydrating oversized items (drop topics.current → distilled → topics).
    // Small items keep full content. Only outliers get stripped.
    let dehydrate_cascade = vec![
        super::chain_engine::DehydrateStep {
            drop: "topics.current".to_string(),
        },
        super::chain_engine::DehydrateStep {
            drop: "distilled".to_string(),
        },
        super::chain_engine::DehydrateStep {
            drop: "topics".to_string(),
        },
    ];

    let budget = ops.tier2.pre_map_prompt_budget;
    // Reserve space for questions + system prompt overhead (~questions_text.len() + 2000)
    let node_budget = budget.saturating_sub(questions_text.len() + 2000);
    let max_batch_items = ops.tier2.pre_map_max_batch_nodes.unwrap_or(200);

    let batches = super::chain_executor::batch_items_by_tokens(
        node_payloads,
        node_budget,
        Some(max_batch_items),
        Some(&dehydrate_cascade),
    );

    let num_batches = batches.len();
    if num_batches > 1 {
        info!(
            total_nodes = lower_layer_nodes.len(),
            num_batches,
            budget = node_budget,
            "pre-mapping: splitting {} nodes into {} batches",
            lower_layer_nodes.len(),
            num_batches
        );
    }

    // ── Prompts ─────────────────────────────────────────────────────────
    let audience_block = match audience {
        Some(aud) if !aud.is_empty() => format!(
            "The questioner is {aud}. ALL evidence is potentially relevant regardless of vocabulary — the answering step handles translation.\n"
        ),
        _ => String::new(),
    };

    let content_type_block = match source_content_type {
        Some(ct) if !ct.is_empty() => format!("The source material is \"{ct}\" content.\n"),
        _ => String::new(),
    };

    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/pre_map.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("audience_block", &audience_block),
                ("content_type_block", &content_type_block),
            ],
        ),
        None => {
            warn!("pre_map.md not found — using inline fallback");
            format!(
                r#"You are mapping questions to candidate evidence nodes. Your job is to determine which nodes from the layer below MIGHT contain relevant evidence for each question.

{audience_block}IMPORTANT: Over-include rather than miss. If a node MIGHT be relevant, include it. The next step will prune irrelevant candidates — a false positive here costs little, but a miss loses evidence permanently.

ALL evidence is potentially relevant regardless of how technical or internal it appears — the answering step handles translation for the audience.

{content_type_block}
Respond with ONLY a JSON object in this exact format:
{{{{
  "mappings": {{{{
    "question_id_1": ["node_id_a", "node_id_b"],
    "question_id_2": ["node_id_c"],
    ...
  }}}}
}}}}

Every question_id from the input MUST appear as a key in the mappings, even if its candidate list is empty."#
            )
        }
    };

    // ── Dispatch each batch and merge candidate maps ────────────────────
    let mut merged_mappings: HashMap<String, Vec<String>> = HashMap::new();
    // Ensure every question has an entry
    for q in questions {
        merged_mappings.entry(q.question_id.clone()).or_default();
    }
    let layer = questions.first().map(|q| q.layer).unwrap_or_default();
    let valid_ids: HashSet<&str> = lower_layer_nodes.iter().map(|n| n.id.as_str()).collect();

    for (batch_idx, batch) in batches.iter().enumerate() {
        let empty_batch = Vec::new();
        let batch_items = batch.as_array().unwrap_or(&empty_batch);
        let nodes_text = batch_items
            .iter()
            .map(|n| {
                let mut parts = vec![
                    format!("  - id: \"{}\"", n["id"].as_str().unwrap_or("")),
                    format!("    headline: \"{}\"", n["headline"].as_str().unwrap_or("")),
                ];
                if let Some(distilled) = n.get("distilled").and_then(|v| v.as_str()) {
                    parts.push(format!("    distilled: \"{}\"", distilled));
                }
                if let Some(topics) = n.get("topics").and_then(|v| v.as_array()) {
                    if !topics.is_empty() {
                        let topic_strs: Vec<String> = topics
                            .iter()
                            .map(|t| {
                                let mut s =
                                    format!("      name: \"{}\"", t["name"].as_str().unwrap_or(""));
                                if let Some(summary) = t.get("summary").and_then(|v| v.as_str()) {
                                    s.push_str(&format!(", summary: \"{}\"", summary));
                                }
                                if let Some(current) = t.get("current").and_then(|v| v.as_str()) {
                                    s.push_str(&format!(", current: \"{}\"", current));
                                }
                                if let Some(entities) = t.get("entities").and_then(|v| v.as_array())
                                {
                                    let ents: Vec<&str> =
                                        entities.iter().filter_map(|e| e.as_str()).collect();
                                    if !ents.is_empty() {
                                        s.push_str(&format!(", entities: [{}]", ents.join(", ")));
                                    }
                                }
                                s
                            })
                            .collect();
                        parts.push(format!("    topics:\n{}", topic_strs.join("\n")));
                    }
                }
                parts.join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n");

        let batch_label = if num_batches > 1 {
            format!(
                " (batch {} of {}, {} nodes)",
                batch_idx + 1,
                num_batches,
                batch_items.len()
            )
        } else {
            String::new()
        };

        let user_prompt = format!(
            "QUESTIONS for this layer:\n{}\n\nNODES from the layer below (candidate evidence){batch_label}:\n{}\n\nFor each question, identify which nodes likely contain relevant evidence. Include uncertain matches.",
            questions_text, nodes_text
        );

        // walker-v3-completion: canonical dispatch via Decision spine.
        // Do not pre-gate on legacy pyramid_tier_routing; evidence_loop
        // lives in walker_provider_* and may resolve through fallback.
        let pre_map_ctx = make_step_ctx_from_llm_config(
            llm_config,
            &format!("evidence_pre_map_{}", batch_idx),
            "evidence_pre_map",
            0,
            Some(batch_idx as i64),
            &system_prompt,
            "evidence_loop",
            None,
            None,
        )
        .await;
        let pre_map_audit_ctx = audit.map(|ctx| AuditContext {
            call_purpose: format!("pre_map_batch_{}", batch_idx),
            step_name: "evidence_pre_map".to_string(),
            ..ctx.clone()
        });
        let response = llm::call_model_unified_with_audit_and_ctx(
            llm_config,
            pre_map_ctx.as_ref(),
            pre_map_audit_ctx.as_ref(),
            &system_prompt,
            &user_prompt,
            ops.tier1.pre_map_temperature,
            ops.tier1.pre_map_max_tokens,
            None,
            llm::LlmCallOptions::default(),
        )
        .await?;

        info!(
            batch = batch_idx,
            batch_nodes = batch_items.len(),
            tokens_in = response.usage.prompt_tokens,
            tokens_out = response.usage.completion_tokens,
            "pre-mapping batch complete"
        );

        // Parse, durably capture this batch's candidate links, then merge into
        // the in-memory map used by answer_questions. The capture is per-batch
        // so a later LLM/DB failure cannot erase earlier pre-map work.
        let json_value = match llm::extract_json(&response.content) {
            Ok(value) => value,
            Err(e) => {
                warn!(
                    batch = batch_idx,
                    error = %e,
                    "Failed to extract JSON from pre-mapping batch response"
                );
                continue;
            }
        };
        let raw = match serde_json::from_value::<PreMapResponse>(json_value) {
            Ok(raw) => raw,
            Err(e) => {
                warn!(
                    batch = batch_idx,
                    error = %e,
                    "Failed to parse pre-mapping batch response"
                );
                continue;
            }
        };
        let valid_batch_mappings = valid_candidate_links_for_batch(&raw.mappings, &valid_ids);
        let captured_links = persist_pre_map_candidate_links(
            pre_map_audit_ctx.as_ref(),
            layer,
            batch_idx,
            response.audit_id,
            valid_batch_mappings,
        )
        .await?;
        if captured_links > 0 {
            info!(
                batch = batch_idx,
                candidate_links = captured_links,
                audit_id = ?response.audit_id,
                "pre-mapping candidate links captured"
            );
        }
        for (q_id, candidates) in raw.mappings {
            merged_mappings.entry(q_id).or_default().extend(candidates);
        }
    }

    // Filter out any node IDs that don't actually exist in the lower layer
    let raw_total: usize = merged_mappings.values().map(|v| v.len()).sum();
    for candidates in merged_mappings.values_mut() {
        candidates.retain(|id| valid_ids.contains(id.as_str()));
        candidates.sort();
        candidates.dedup();
    }

    let total_candidates: usize = merged_mappings.values().map(|v| v.len()).sum();
    if total_candidates < raw_total {
        warn!(
            raw_total,
            resolved = total_candidates,
            dropped = raw_total - total_candidates,
            "pre-mapping: some LLM-returned IDs did not match any node in the lower layer"
        );
    }
    info!(
        total_candidates,
        questions = questions.len(),
        batches = num_batches,
        "pre-mapping complete"
    );

    Ok(CandidateMap {
        mappings: merged_mappings,
    })
}

/// Internal deserialization target for the pre-mapping LLM response.
#[derive(Deserialize)]
struct PreMapResponse {
    mappings: HashMap<String, Vec<String>>,
}

// ── Step 3.2: Vertical Answering ─────────────────────────────────────────────

/// Answer all questions in parallel using their candidate evidence.
///
/// For each question:
///   1. Look up candidates from the CandidateMap
///   2. Fetch full node content for each candidate
///   3. LLM call to evaluate evidence and synthesize an answer
///   4. Parse KEEP/DISCONNECT/MISSING verdicts
///   5. Save evidence links to pyramid_evidence table
///   6. Save the answered node to pyramid_nodes
///
/// Returns the answered nodes with their evidence links and any MISSING reports.
/// The caller is responsible for persisting results to the database (e.g. via
/// spawn_blocking), which solves the `&Connection` / `!Send` problem.
///
/// Parallel, 5x concurrency via tokio::sync::Semaphore.
pub async fn answer_questions(
    questions: &[LayerQuestion],
    candidate_map: &CandidateMap,
    all_nodes: &[PyramidNode],
    synthesis_prompt: Option<&str>,
    audience: Option<&str>,
    llm_config: &LlmConfig,
    slug: &str,
    answer_slug: &str,
    chains_dir: Option<&PathBuf>,
    source_content_type: Option<&str>,
    ops: &OperationalConfig,
    audit: Option<&AuditContext>,
    on_answer: Option<&tokio::sync::mpsc::Sender<()>>,
) -> Result<AnswerBatchResult> {
    if questions.is_empty() {
        return Ok(AnswerBatchResult {
            answered: Vec::new(),
            failed: Vec::new(),
        });
    }

    // Phase 13: capture bus + build_id once for all event emissions
    // in this function. `bus_for_events` is None when the config has
    // no cache plumbing (unit tests), in which case the emit helpers
    // all no-op.
    let bus_for_events = llm_config
        .cache_access
        .as_ref()
        .and_then(|ca| ca.bus.clone());
    let build_id_for_events = llm_config
        .cache_access
        .as_ref()
        .map(|ca| ca.build_id.clone())
        .unwrap_or_else(|| format!("{}-evidence", slug));
    let step_name_for_events = "answer_questions".to_string();

    // ── Phase 12: Triage gate ───────────────────────────────────────
    //
    // Before dispatching questions to the expensive answering path,
    // run each question through the triage DSL against the active
    // evidence_policy. Questions with no policy fall through as
    // "Answer" (the default). Questions with a matching "defer" rule
    // go to pyramid_deferred_questions; questions with a matching
    // "skip" rule are dropped.
    //
    // The triage gate activates only when llm_config.cache_access is
    // populated (i.e. we have a db_path to read the policy from).
    // Unit tests that don't set cache_access keep the pre-Phase-12
    // behavior.

    // Phase 13: emit EvidenceProcessing { action: "triage" } at the
    // start of the triage pass.
    if let Some(bus) = bus_for_events.as_ref() {
        let _ = bus.tx.send(crate::pyramid::event_bus::TaggedBuildEvent {
            slug: slug.to_string(),
            kind: crate::pyramid::event_bus::TaggedKind::EvidenceProcessing {
                slug: slug.to_string(),
                build_id: build_id_for_events.clone(),
                step_name: step_name_for_events.clone(),
                question_count: questions.len() as i64,
                action: "triage".to_string(),
                model_tier: "evidence_loop".to_string(),
            },
        });
    }

    let (triaged_questions, triage_stats) = if let Some(ca) = llm_config.cache_access.as_ref() {
        let db_path = ca.db_path.to_string();
        let slug_for_triage = slug.to_string();
        let questions_for_triage: Vec<LayerQuestion> = questions.to_vec();
        // Run the triage pass in spawn_blocking since it opens a DB
        // connection. This is a fast synchronous DB read + DSL
        // evaluation — no LLM call in the MVP (triage LLM
        // classification is deferred per the workstream prompt's
        // "make most load-bearing reasonable call" guidance).
        let triage_result = tokio::task::spawn_blocking(move || {
            run_triage_gate(&db_path, &slug_for_triage, &questions_for_triage)
        })
        .await
        .map_err(|e| anyhow!("triage gate join error: {}", e))??;

        // Phase 13: emit one TriageDecision event per question. This
        // gives the UI a concrete per-question view — the spec says
        // every decision is its own row.
        if let Some(bus) = bus_for_events.as_ref() {
            for rec in &triage_result.decisions {
                let _ = bus.tx.send(crate::pyramid::event_bus::TaggedBuildEvent {
                    slug: slug.to_string(),
                    kind: crate::pyramid::event_bus::TaggedKind::TriageDecision {
                        slug: slug.to_string(),
                        build_id: build_id_for_events.clone(),
                        step_name: step_name_for_events.clone(),
                        item_id: rec.question_id.clone(),
                        decision: rec.decision_tag.clone(),
                        reason: rec.reason.clone(),
                    },
                });
            }
        }

        (triage_result.answer_questions, triage_result.stats)
    } else {
        // No cache_access → triage disabled; all questions answered.
        (
            questions.to_vec(),
            TriageStats {
                evaluated: questions.len(),
                answered: questions.len(),
                deferred: 0,
                skipped: 0,
            },
        )
    };

    info!(
        evaluated = triage_stats.evaluated,
        answered = triage_stats.answered,
        deferred = triage_stats.deferred,
        skipped = triage_stats.skipped,
        "Phase 12 triage gate partitioned questions"
    );

    if triaged_questions.is_empty() {
        return Ok(AnswerBatchResult {
            answered: Vec::new(),
            failed: Vec::new(),
        });
    }
    // Replace the `questions` binding with the triaged subset so the
    // downstream parallel-answering loop only runs on the `Answer`
    // bucket. `questions` shadows the original slice with a
    // `Vec<LayerQuestion>`.
    let questions: &[LayerQuestion] = &triaged_questions;

    // Phase 13: emit EvidenceProcessing { action: "answer" } at the
    // start of the parallel answering loop. `question_count` is the
    // post-triage size; the UI can diff against the earlier "triage"
    // event to render the "N deferred, M answered" split.
    if let Some(bus) = bus_for_events.as_ref() {
        let _ = bus.tx.send(crate::pyramid::event_bus::TaggedBuildEvent {
            slug: slug.to_string(),
            kind: crate::pyramid::event_bus::TaggedKind::EvidenceProcessing {
                slug: slug.to_string(),
                build_id: build_id_for_events.clone(),
                step_name: step_name_for_events.clone(),
                question_count: questions.len() as i64,
                action: "answer".to_string(),
                model_tier: "synth_heavy".to_string(),
            },
        });
    }

    // Build a lookup map for all nodes by ORIGINAL ID
    let node_map: HashMap<&str, &PyramidNode> =
        all_nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let semaphore = Arc::new(Semaphore::new(ops.tier1.answer_concurrency));
    let llm_config = Arc::new(llm_config.clone());
    let ops = Arc::new(ops.clone());
    let slug = slug.to_string();
    let synthesis_prompt = synthesis_prompt.map(|s| s.to_string());
    let audience = audience.map(|s| s.to_string());
    let answer_temperature = ops.tier1.answer_temperature;
    let answer_max_tokens = ops.tier1.answer_max_tokens;

    // Prepare per-question work items, rewriting cross-slug node IDs to handle-paths
    let answer_slug_owned = answer_slug.to_string();
    let work_items: Vec<AnswerWorkItem> = questions
        .iter()
        .enumerate()
        .map(|(idx, q)| {
            let candidate_ids = candidate_map
                .mappings
                .get(&q.question_id)
                .cloned()
                .unwrap_or_default();

            // Resolve candidate IDs to full node data, rewriting IDs for cross-slug candidates
            let candidate_nodes: Vec<PyramidNode> = candidate_ids
                .iter()
                .filter_map(|id| {
                    node_map.get(id.as_str()).map(|n| {
                        let mut node = (*n).clone();
                        // If candidate comes from a different slug, rewrite its ID to handle-path
                        if node.slug != answer_slug_owned {
                            node.id = db::format_handle_path(&node.slug, node.depth, &node.id);
                        }
                        node
                    })
                })
                .collect();

            AnswerWorkItem {
                question: q.clone(),
                candidate_nodes,
                seq_index: idx,
            }
        })
        .collect();

    // Spawn parallel tasks — each returns (question_meta, Result<AnsweredNode>)
    let audit_owned = audit.cloned();
    let mut handles = Vec::new();
    for work in work_items {
        let semaphore = semaphore.clone();
        let llm_config = llm_config.clone();
        let ops = ops.clone();
        let slug = slug.clone();
        let answer_slug = answer_slug_owned.clone();
        let synthesis_prompt = synthesis_prompt.clone();
        let audience = audience.clone();
        let chains_dir_owned = chains_dir.cloned();
        let source_ct = source_content_type.map(|s| s.to_string());
        let q_id = work.question.question_id.clone();
        let q_text = work.question.question_text.clone();
        let q_layer = work.question.layer;
        let task_audit = audit_owned.clone();

        let seq_index = work.seq_index;
        let handle = tokio::spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("answer semaphore should remain open");

            let result = answer_single_question(
                &work.question,
                &work.candidate_nodes,
                seq_index,
                synthesis_prompt.as_deref(),
                audience.as_deref(),
                &llm_config,
                &slug,
                &answer_slug,
                answer_temperature,
                answer_max_tokens,
                chains_dir_owned.as_ref(),
                source_ct.as_deref(),
                task_audit.as_ref(),
                &ops,
            )
            .await;
            (q_id, q_text, q_layer, result)
        });

        handles.push(handle);
    }

    // Collect results — NO DB writes here. The caller persists via spawn_blocking.
    let mut answered_nodes = Vec::new();
    let mut failed_questions = Vec::new();
    let mut total_evidence = 0usize;
    let mut total_missing = 0usize;

    for handle in handles {
        let (q_id, q_text, q_layer, result) = handle
            .await
            .map_err(|e| anyhow!("Answer task panicked: {}", e))?;
        match result {
            Ok(answered) => {
                total_evidence += answered.evidence.len();
                total_missing += answered.missing.len();

                // Phase 3a: emit VerdictProduced for each evidence link
                if let Some(bus) = bus_for_events.as_ref() {
                    for ev in &answered.evidence {
                        let source_headline = node_map
                            .get(ev.source_node_id.as_str())
                            .map(|n| n.headline.clone());
                        let target_headline = Some(answered.node.headline.clone());
                        let _ = bus.tx.send(crate::pyramid::event_bus::TaggedBuildEvent {
                            slug: slug.clone(),
                            kind: crate::pyramid::event_bus::TaggedKind::VerdictProduced {
                                slug: slug.clone(),
                                build_id: build_id_for_events.clone(),
                                step_name: step_name_for_events.clone(),
                                node_id: ev.target_node_id.clone(),
                                verdict: ev.verdict.as_str().to_string(),
                                source_id: ev.source_node_id.clone(),
                                weight: ev.weight,
                                source_headline,
                                target_headline,
                            },
                        });
                    }
                }

                answered_nodes.push(answered);
                if let Some(tx) = on_answer {
                    let _ = tx.send(()).await;
                }
            }
            Err(e) => {
                warn!(question_id = %q_id, error = %e, "Failed to answer question — recording as gap report");
                failed_questions.push(FailedQuestion {
                    question_id: q_id,
                    question_text: q_text,
                    layer: q_layer,
                    error: e.to_string(),
                });
            }
        }
    }

    info!(
        answered = answered_nodes.len(),
        failed = failed_questions.len(),
        total_evidence,
        total_missing,
        "answering complete"
    );

    Ok(AnswerBatchResult {
        answered: answered_nodes,
        failed: failed_questions,
    })
}

// ── Internal Types ───────────────────────────────────────────────────────────

struct AnswerWorkItem {
    question: LayerQuestion,
    candidate_nodes: Vec<PyramidNode>,
    /// Sequential index within this batch, used to generate short deterministic
    /// node IDs (e.g. L2-003) instead of UUIDs.
    seq_index: usize,
}

// ── Per-Question Answering ───────────────────────────────────────────────────

/// Answer a single question using its candidate evidence nodes.
///
/// Returns an AnsweredNode containing the synthesized node, evidence links, and
/// any MISSING evidence reports.
async fn answer_single_question(
    question: &LayerQuestion,
    candidate_nodes: &[PyramidNode],
    seq_index: usize,
    synthesis_prompt: Option<&str>,
    audience: Option<&str>,
    llm_config: &LlmConfig,
    slug: &str,
    answer_slug: &str,
    answer_temperature: f32,
    answer_max_tokens: usize,
    chains_dir: Option<&PathBuf>,
    source_content_type: Option<&str>,
    audit: Option<&AuditContext>,
    ops: &OperationalConfig,
) -> Result<AnsweredNode> {
    // Use short sequential IDs (L2-003) matching the pattern used by build.rs.
    // UUIDs are impossible for LLMs to reproduce in downstream pre-mapping steps.
    let node_id = format!("L{}-{:03}", question.layer, seq_index);

    // Short-circuit: no candidates → skip LLM, emit placeholder + MISSING verdict
    if candidate_nodes.is_empty() {
        info!(
            slug,
            question = %question.question_text,
            layer = question.layer,
            "Zero candidates — skipping LLM, emitting MISSING for gap_processing"
        );
        return Ok(AnsweredNode {
            node: PyramidNode {
                id: node_id,
                slug: answer_slug.to_string(),
                depth: question.layer,
                chunk_index: None,
                headline: question.question_text.clone(),
                distilled: "Awaiting evidence — no candidates mapped during pre-mapping."
                    .to_string(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                superseded_by: None,
                build_id: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            },
            audit_id: None,
            provenance_kind: ProvenanceKind::StubLegacy,
            evidence: vec![],
            missing: vec![format!(
                "No candidate evidence was mapped during pre-mapping for question: {}",
                question.question_text
            )],
        });
    }

    // Build candidate_map keyed by the IDs shown to the LLM (handle-paths for cross-slug, bare for same-slug).
    // Node IDs have already been rewritten by answer_questions before reaching here.
    let candidate_map: HashMap<String, &PyramidNode> =
        candidate_nodes.iter().map(|n| (n.id.clone(), n)).collect();

    // ── Prompts (shared across single-pass and batched paths) ──────────
    let synthesis_guidance = synthesis_prompt.unwrap_or("");

    let audience_block = match audience {
        Some(aud) if !aud.is_empty() => format!(
            "You are writing for {aud}. ALL technical terms from the evidence MUST be translated to plain language.\nThe reader should NEVER encounter framework names, file names, function names, API terms, or programming concepts unless they specifically asked about development.\nExtract the USER-FACING MEANING from technical evidence and express THAT.\n\n"
        ),
        _ => String::new(),
    };

    let content_type_block = match source_content_type {
        Some(ct) if !ct.is_empty() => format!("The source material is \"{ct}\" content.\n"),
        _ => String::new(),
    };

    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/answer.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("audience_block", &audience_block),
                ("synthesis_prompt", synthesis_guidance),
                ("content_type_block", &content_type_block),
            ],
        ),
        None => {
            warn!("answer.md not found — using inline fallback");
            format!(
                r#"{audience_block}You are answering a knowledge pyramid question using candidate evidence from the layer below.

For each candidate node, you MUST report a verdict:
- KEEP(weight, reason) — this evidence is relevant. Weight 0.0-1.0 indicates how central it is.
- DISCONNECT(reason) — this evidence was a false positive from pre-mapping, not actually relevant.
- MISSING(description) — describe evidence you wish you had but don't.

Then synthesize your answer to the question using ONLY the KEEP evidence.

Focus your synthesis on your STRONGEST evidence — the nodes that most directly answer the question.
You do not need to mention every KEEP node. A focused answer drawing from your best sources is better than a sprawling answer trying to mention everything.

{synthesis_guidance}

{content_type_block}

Respond with ONLY a JSON object:
{{{{
  "headline": "short headline for this answer (max 120 chars)",
  "distilled": "2-4 sentence synthesis answering the question",
  "topics": [
    {{{{"name": "topic_name", "current": "what we know about this topic"}}}}
  ],
  "verdicts": [
    {{{{"node_id": "...", "verdict": "KEEP", "weight": 0.85, "reason": "..."}}}},
    {{{{"node_id": "...", "verdict": "DISCONNECT", "reason": "..."}}}},
    {{{{"node_id": "...", "verdict": "KEEP", "weight": 0.3, "reason": "..."}}}}
  ],
  "missing": [
    "description of evidence we wish we had"
  ],
  "corrections": [],
  "decisions": [],
  "terms": [],
  "dead_ends": []
}}}}"#
            )
        }
    };

    // ── Build candidate payloads as JSON Values for token estimation + batching ──
    let candidate_payloads: Vec<serde_json::Value> = candidate_nodes
        .iter()
        .map(|n| {
            serde_json::json!({
                "id": n.id,
                "headline": n.headline,
                "distilled": n.distilled,
                "topics": n.topics.iter().map(|t| {
                    serde_json::json!({
                        "name": &t.name,
                        "current": &t.current,
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();

    // Estimate total evidence tokens
    let total_evidence_tokens: usize = candidate_payloads
        .iter()
        .map(|p| {
            serde_json::to_string(p)
                .map(|s| s.len().div_ceil(4))
                .unwrap_or(0)
        })
        .sum();

    let answer_budget = ops.tier2.answer_prompt_budget;
    // Reserve ~2K tokens for system prompt + question framing
    let evidence_budget = answer_budget.saturating_sub(2000);

    let needs_batching = total_evidence_tokens > evidence_budget;

    if needs_batching {
        info!(
            question_id = %question.question_id,
            candidates = candidate_nodes.len(),
            total_evidence_tokens,
            evidence_budget,
            "answer overflow — batching candidates"
        );
    }

    // ── Dehydrate cascade for oversized individual candidates ───────────
    let dehydrate_cascade = vec![
        super::chain_engine::DehydrateStep {
            drop: "topics.current".to_string(),
        },
        super::chain_engine::DehydrateStep {
            drop: "distilled".to_string(),
        },
        super::chain_engine::DehydrateStep {
            drop: "topics".to_string(),
        },
    ];

    let batches = if needs_batching {
        super::chain_executor::batch_items_by_tokens(
            candidate_payloads,
            evidence_budget,
            None,
            Some(&dehydrate_cascade),
        )
    } else {
        // Single batch — apply dehydration only if any individual item is enormous
        super::chain_executor::batch_items_by_tokens(
            candidate_payloads,
            evidence_budget,
            None,
            Some(&dehydrate_cascade),
        )
    };

    let num_batches = batches.len();

    // ── Call LLM per batch ──────────────────────────────────────────────
    let mut batch_results: Vec<RawAnswerResponse> = Vec::new();
    let mut direct_answer_audit_id: Option<i64> = None;

    for (batch_idx, batch) in batches.iter().enumerate() {
        let empty = Vec::new();
        let batch_items = batch.as_array().unwrap_or(&empty);

        // Render evidence context from (possibly dehydrated) payloads
        let evidence_context: String = batch_items
            .iter()
            .map(|n| {
                let id = n["id"].as_str().unwrap_or("");
                let headline = n["headline"].as_str().unwrap_or("");
                let mut parts = format!("--- NODE {} ---\nHeadline: {}\n", id, headline);
                if let Some(distilled) = n.get("distilled").and_then(|v| v.as_str()) {
                    parts.push_str(&format!("Distilled: {}\n", distilled));
                }
                if let Some(topics) = n.get("topics").and_then(|v| v.as_array()) {
                    if !topics.is_empty() {
                        let topic_str: String = topics
                            .iter()
                            .map(|t| {
                                let name = t["name"].as_str().unwrap_or("");
                                if let Some(current) = t.get("current").and_then(|v| v.as_str()) {
                                    format!("{}: {}", name, current)
                                } else {
                                    name.to_string()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        parts.push_str(&format!("Topics: {}\n", topic_str));
                    }
                }
                parts
            })
            .collect::<Vec<_>>()
            .join("\n");

        let batch_label = if num_batches > 1 {
            format!(
                " (batch {} of {}, {} candidates)",
                batch_idx + 1,
                num_batches,
                batch_items.len()
            )
        } else {
            String::new()
        };

        let user_prompt = format!(
            "QUESTION (id: {}):\n{}\n\nAbout: {}\nCreates: {}\n\nCANDIDATE EVIDENCE{batch_label}:\n{}\n\nEvaluate each candidate, produce verdicts, and synthesize your answer.",
            question.question_id,
            question.question_text,
            question.about,
            question.creates,
            evidence_context
        );

        // Phase 18b L8 retrofit: build a cache-usable StepContext from
        // llm_config.cache_access (populated by the build pipeline via
        // clone_with_cache_access) and thread it together with the
        // optional AuditContext through the unified entry point. This
        // collapses the previous "audit branch (no cache) vs non-audit
        // branch (with cache)" split into a single call where audited
        // builds also benefit from the Phase 6 content-addressable
        // cache. Audited cache hits write a `cache_hit = 1` audit row
        // so the audit trail stays contiguous.
        // walker-v3-completion: canonical dispatch via Decision spine.
        let answer_ctx = make_step_ctx_from_llm_config(
            llm_config,
            &format!("evidence_answer_batch_{}", batch_idx),
            "evidence_answer",
            question.layer as i64,
            Some(batch_idx as i64),
            &system_prompt,
            "evidence_loop",
            None,
            None,
        )
        .await;
        let answer_audit_ctx = audit.map(|ctx| {
            ctx.for_node(
                &node_id,
                &format!("answer_batch_{}", batch_idx),
                question.layer as i64,
            )
        });
        let response = llm::call_model_unified_with_audit_and_ctx(
            llm_config,
            answer_ctx.as_ref(),
            answer_audit_ctx.as_ref(),
            &system_prompt,
            &user_prompt,
            answer_temperature,
            answer_max_tokens,
            None,
            llm::LlmCallOptions::default(),
        )
        .await?;
        if num_batches == 1 {
            direct_answer_audit_id = response.audit_id;
        }

        info!(
            question_id = %question.question_id,
            batch = batch_idx,
            batch_candidates = batch_items.len(),
            tokens_in = response.usage.prompt_tokens,
            tokens_out = response.usage.completion_tokens,
            "answer batch LLM call complete"
        );

        let json_value = llm::extract_json(&response.content)?;
        let raw: RawAnswerResponse = serde_json::from_value(json_value).map_err(|e| {
            anyhow!(
                "Failed to parse answer response for {} batch {}: {} — raw: {}",
                question.question_id,
                batch_idx,
                e,
                &response.content[..response.content.len().min(400)]
            )
        })?;
        batch_results.push(raw);
    }

    // ── Merge batch results ─────────────────────────────────────────────
    let single_batch_answer = batch_results.len() == 1;
    let raw = if single_batch_answer {
        // Single batch — no merge needed
        batch_results.into_iter().next().unwrap()
    } else {
        // Multiple batches — merge via LLM
        info!(
            question_id = %question.question_id,
            num_batches,
            "merging {} answer batches",
            num_batches
        );
        merge_answer_batches(
            question,
            &batch_results,
            &audience_block,
            synthesis_guidance,
            &content_type_block,
            llm_config,
            answer_temperature,
            answer_max_tokens,
            chains_dir,
            audit,
            &node_id,
            ops,
        )
        .await?
    };

    // ── Build PyramidNode ───────────────────────────────────────────────
    let topics = raw
        .topics
        .into_iter()
        .map(|t| super::types::Topic {
            name: t.name,
            current: t.current,
            entities: Vec::new(),
            corrections: Vec::new(),
            decisions: Vec::new(),
            extra: serde_json::Map::new(),
        })
        .collect();

    let corrections = raw
        .corrections
        .unwrap_or_default()
        .into_iter()
        .map(|c| super::types::Correction {
            wrong: c.wrong,
            right: c.right,
            who: c.who.unwrap_or_default(),
        })
        .collect();

    let decisions = raw
        .decisions
        .unwrap_or_default()
        .into_iter()
        .map(|d| super::types::Decision {
            decided: d.decided,
            why: d.why,
            rejected: d.rejected.unwrap_or_default(),
            ..Default::default()
        })
        .collect();

    let terms = raw
        .terms
        .unwrap_or_default()
        .into_iter()
        .map(|t| super::types::Term {
            term: t.term,
            definition: t.definition,
        })
        .collect();

    // ── Build EvidenceLinks (KEEP and DISCONNECT only) ────────────────
    // MISSING verdicts are NOT evidence links — they have fabricated source_node_ids.
    // Missing evidence is captured via raw.missing and saved as gap reports by the caller.
    //
    // Resolve verdict node_ids against the candidate_map. The LLM sees handle-path IDs
    // for cross-slug candidates, so it should return them. If a verdict references an
    // unknown ID, skip it with a warning.
    let mut evidence: Vec<EvidenceLink> = Vec::new();
    let mut children: Vec<String> = Vec::new();

    for v in &raw.verdicts {
        let verdict = match v.verdict.to_uppercase().as_str() {
            "KEEP" => EvidenceVerdict::Keep,
            "DISCONNECT" => EvidenceVerdict::Disconnect,
            "MISSING" => continue, // Skip — tracked via raw.missing gap reports
            other => {
                warn!(verdict = other, "Unknown verdict, defaulting to Keep");
                EvidenceVerdict::Keep
            }
        };

        // Resolve against candidate_map — ensures we only accept IDs we showed the LLM
        if !candidate_map.contains_key(&v.node_id) {
            warn!(
                node_id = %v.node_id,
                question_id = %question.question_id,
                "LLM returned unknown node_id, skipping"
            );
            continue;
        }

        // Use the candidate_map key as source_node_id (handle-path for cross-slug, bare for same-slug)
        let source_node_id = v.node_id.clone();

        if verdict == EvidenceVerdict::Keep {
            children.push(source_node_id.clone());
        }

        let weight = if verdict == EvidenceVerdict::Keep {
            Some(v.weight.unwrap_or(0.5).clamp(0.0, 1.0))
        } else {
            None
        };

        evidence.push(EvidenceLink {
            slug: answer_slug.to_string(),
            source_node_id,
            target_node_id: node_id.clone(),
            verdict,
            weight,
            reason: v.reason.clone(),
            build_id: None,
            live: Some(true),
        });
    }

    let node = PyramidNode {
        id: node_id.clone(),
        slug: answer_slug.to_string(),
        depth: question.layer,
        chunk_index: None,
        headline: raw.headline,
        distilled: raw.distilled,
        topics,
        corrections,
        decisions,
        terms,
        dead_ends: raw.dead_ends.unwrap_or_default(),
        self_prompt: question.question_text.clone(),
        source_question_id: Some(question.question_id.clone()),
        children,
        parent_id: None,
        superseded_by: None,
        build_id: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        ..Default::default()
    };

    let missing = raw.missing.unwrap_or_default();

    Ok(AnsweredNode {
        node,
        audit_id: if single_batch_answer {
            direct_answer_audit_id
        } else {
            None
        },
        provenance_kind: ProvenanceKind::Llm,
        evidence,
        missing,
    })
}

// ── Answer Batch Merge ──────────────────────────────────────────────────────

/// Merge multiple batch answer results into a single unified RawAnswerResponse.
/// Uses an LLM merge call to synthesize partial answers from batched evidence.
/// Serialize one batch result as a complete JSON item carrying ALL its fields
/// (synthesis + verdicts + topics + structural notes). Used as input to the
/// merge LLM call so it can reconcile across batches with full context.
fn serialize_batch_full(idx: usize, batch: &RawAnswerResponse) -> Value {
    let topics_val: Vec<Value> = batch
        .topics
        .iter()
        .map(|t| serde_json::json!({"name": t.name, "current": t.current}))
        .collect();
    let verdicts_val: Vec<Value> = batch
        .verdicts
        .iter()
        .map(|v| {
            let mut obj = serde_json::json!({
                "node_id": v.node_id,
                "verdict": v.verdict,
            });
            if let Some(w) = v.weight {
                obj["weight"] = serde_json::json!(w);
            }
            if let Some(ref r) = v.reason {
                obj["reason"] = serde_json::json!(r);
            }
            obj
        })
        .collect();
    let corrections_val: Vec<Value> = batch
        .corrections
        .as_ref()
        .map(|cs| {
            cs.iter()
                .map(|c| serde_json::json!({"wrong": c.wrong, "right": c.right, "who": c.who}))
                .collect()
        })
        .unwrap_or_default();
    let decisions_val: Vec<Value> = batch
        .decisions
        .as_ref()
        .map(|ds| {
            ds.iter()
                .map(|d| serde_json::json!({"decided": d.decided, "why": d.why, "rejected": d.rejected}))
                .collect()
        })
        .unwrap_or_default();
    let terms_val: Vec<Value> = batch
        .terms
        .as_ref()
        .map(|ts| {
            ts.iter()
                .map(|t| serde_json::json!({"term": t.term, "definition": t.definition}))
                .collect()
        })
        .unwrap_or_default();

    serde_json::json!({
        "batch_index": idx + 1,
        "headline": batch.headline,
        "distilled": batch.distilled,
        "topics": topics_val,
        "verdicts": verdicts_val,
        "missing": batch.missing.clone().unwrap_or_default(),
        "corrections": corrections_val,
        "decisions": decisions_val,
        "terms": terms_val,
        "dead_ends": batch.dead_ends.clone().unwrap_or_default(),
    })
}

async fn merge_answer_batches(
    question: &LayerQuestion,
    batch_results: &[RawAnswerResponse],
    audience_block: &str,
    synthesis_guidance: &str,
    content_type_block: &str,
    llm_config: &LlmConfig,
    answer_temperature: f32,
    answer_max_tokens: usize,
    chains_dir: Option<&PathBuf>,
    audit: Option<&AuditContext>,
    node_id: &str,
    ops: &OperationalConfig,
) -> Result<RawAnswerResponse> {
    // Pillar 44: this step's input scales with the number of batches and the
    // size of each batch's verdicts/topics/corrections/etc. Use the
    // token-aware batching + auto-dehydration pattern. The LLM still does the
    // reconciliation (Pillar 37) — we shape the input to fit the pipe, we do
    // not strip fields and replace intelligence with rules.

    let items: Vec<Value> = batch_results
        .iter()
        .enumerate()
        .map(|(idx, batch)| serialize_batch_full(idx, batch))
        .collect();

    // Dehydration cascade: drop the heaviest fields first (verdict reasons,
    // then full verdicts, then topic.current, then topics). The LLM still
    // sees node IDs and verdict labels even after dehydration so it can
    // reconcile by reference.
    let dehydrate_cascade = vec![
        super::chain_engine::DehydrateStep {
            drop: "verdicts.reason".to_string(),
        },
        super::chain_engine::DehydrateStep {
            drop: "topics.current".to_string(),
        },
        super::chain_engine::DehydrateStep {
            drop: "verdicts".to_string(),
        },
        super::chain_engine::DehydrateStep {
            drop: "topics".to_string(),
        },
        super::chain_engine::DehydrateStep {
            drop: "distilled".to_string(),
        },
    ];

    // Overhead: question text + system prompt + JSON syntax slack
    let overhead = question.question_text.len() / 4 + 4000;
    let budget = ops.tier2.answer_prompt_budget.saturating_sub(overhead);

    let packed = super::chain_executor::batch_items_by_tokens(
        items,
        budget,
        None, // no item-count cap; let token budget decide
        Some(&dehydrate_cascade),
    );

    // If everything fits in one batch, do a single merge call.
    // If dehydration produced multiple batches, do nested pairwise merging:
    // merge each batch group into a partial result, then merge those results.
    if packed.len() == 1 {
        let items_in_batch = match packed.into_iter().next() {
            Some(Value::Array(items)) => items,
            _ => {
                return Err(anyhow!(
                    "merge: batch_items_by_tokens returned malformed batch"
                ))
            }
        };
        return single_merge_call(
            question,
            &items_in_batch,
            audience_block,
            synthesis_guidance,
            content_type_block,
            llm_config,
            answer_temperature,
            answer_max_tokens,
            chains_dir,
            audit,
            node_id,
        )
        .await;
    }

    // Multi-batch path: merge each group into a partial RawAnswerResponse,
    // then recurse to merge the partials. This is the "nested merge"
    // requirement of Pillar 44 when even after dehydration the input still
    // exceeds the budget.
    info!(
        question_id = %question.question_id,
        groups = packed.len(),
        "answer merge nested: dehydration cascade still exceeded budget, merging in groups"
    );

    let mut partial_results: Vec<RawAnswerResponse> = Vec::with_capacity(packed.len());
    for (gidx, group) in packed.into_iter().enumerate() {
        let group_items = match group {
            Value::Array(items) => items,
            other => vec![other],
        };
        let partial = single_merge_call(
            question,
            &group_items,
            audience_block,
            synthesis_guidance,
            content_type_block,
            llm_config,
            answer_temperature,
            answer_max_tokens,
            chains_dir,
            audit,
            &format!("{}-mg{}", node_id, gidx),
        )
        .await?;
        partial_results.push(partial);
    }

    // Recurse: merge the partials. If the partials are also too large after
    // dehydration, this will recurse again and merge in larger groupings.
    Box::pin(merge_answer_batches(
        question,
        &partial_results,
        audience_block,
        synthesis_guidance,
        content_type_block,
        llm_config,
        answer_temperature,
        answer_max_tokens,
        chains_dir,
        audit,
        node_id,
        ops,
    ))
    .await
}

async fn single_merge_call(
    question: &LayerQuestion,
    items: &[Value],
    audience_block: &str,
    synthesis_guidance: &str,
    content_type_block: &str,
    llm_config: &LlmConfig,
    answer_temperature: f32,
    answer_max_tokens: usize,
    chains_dir: Option<&PathBuf>,
    audit: Option<&AuditContext>,
    node_id: &str,
) -> Result<RawAnswerResponse> {
    let merge_system = match chains_dir
        .map(|d| d.join("prompts/question/answer_merge.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("audience_block", audience_block),
                ("synthesis_prompt", synthesis_guidance),
                ("content_type_block", content_type_block),
            ],
        ),
        None => {
            warn!("answer_merge.md not found — using inline fallback");
            format!(
                r#"{audience_block}You are merging partial answers to a knowledge pyramid question. The evidence was too large for a single pass, so it was split into batches. Each batch produced verdicts and a partial synthesis. Your job is to produce a SINGLE unified answer that reconciles across batches.

### MERGE RULES
1. VERDICTS: Combine across batches. When the same node_id appears in multiple batches with different verdicts, judge which is more accurate based on the reasoning given. When verdicts agree, keep one. The verdicts you keep are the final verdicts for this answer.
2. SYNTHESIS: Read all partial syntheses and produce ONE unified synthesis that covers all dimensions. Synthesize across batches as if you had seen all evidence at once.
3. TOPICS: Merge topics from all batches. Deduplicate by name, keeping the richest "current" text.
4. MISSING/CORRECTIONS/DECISIONS/TERMS/DEAD_ENDS: Union all entries, deduplicate by meaning (not just exact string match).

{synthesis_guidance}

{content_type_block}

Respond with ONLY a JSON object:
{{{{
  "headline": "short headline",
  "distilled": "unified synthesis",
  "topics": [{{{{"name": "...", "current": "..."}}}}],
  "verdicts": [{{{{"node_id": "...", "verdict": "KEEP", "weight": 0.85, "reason": "..."}}}}],
  "missing": [],
  "corrections": [],
  "decisions": [],
  "terms": [],
  "dead_ends": []
}}}}"#
            )
        }
    };

    let merge_user = format!(
        "QUESTION (id: {}):\n{}\n\nAbout: {}\nCreates: {}\n\nPARTIAL ANSWERS FROM BATCHES (each carries its own verdicts, synthesis, topics, and structural notes — reconcile across them):\n{}\n\nProduce the unified answer.",
        question.question_id,
        question.question_text,
        question.about,
        question.creates,
        serde_json::to_string_pretty(items).unwrap_or_default(),
    );

    // walker-v3-completion: canonical dispatch via Decision spine.
    let merge_ctx = make_step_ctx_from_llm_config(
        llm_config,
        "evidence_answer_merge",
        "evidence_answer_merge",
        question.layer as i64,
        None,
        &merge_system,
        "evidence_loop",
        None,
        None,
    )
    .await;
    let merge_audit_ctx =
        audit.map(|ctx| ctx.for_node(node_id, "answer_merge", question.layer as i64));
    let response = llm::call_model_unified_with_audit_and_ctx(
        llm_config,
        merge_ctx.as_ref(),
        merge_audit_ctx.as_ref(),
        &merge_system,
        &merge_user,
        answer_temperature,
        answer_max_tokens,
        None,
        llm::LlmCallOptions::default(),
    )
    .await?;

    info!(
        question_id = %question.question_id,
        tokens_in = response.usage.prompt_tokens,
        tokens_out = response.usage.completion_tokens,
        "answer merge LLM call complete"
    );

    let json_value = llm::extract_json(&response.content)?;
    let raw: RawAnswerResponse = serde_json::from_value(json_value).map_err(|e| {
        anyhow!(
            "Failed to parse merge response for {}: {} — raw: {}",
            question.question_id,
            e,
            &response.content[..response.content.len().min(400)]
        )
    })?;

    Ok(raw)
}

// ── Raw LLM Response Types (internal) ────────────────────────────────────────

#[derive(Deserialize)]
struct RawAnswerResponse {
    headline: String,
    distilled: String,
    #[serde(default)]
    topics: Vec<RawTopic>,
    verdicts: Vec<RawVerdict>,
    #[serde(default)]
    missing: Option<Vec<String>>,
    #[serde(default)]
    corrections: Option<Vec<RawCorrection>>,
    #[serde(default)]
    decisions: Option<Vec<RawDecision>>,
    #[serde(default)]
    terms: Option<Vec<RawTerm>>,
    #[serde(default)]
    dead_ends: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RawTopic {
    name: String,
    current: String,
}

#[derive(Deserialize, Clone)]
struct RawVerdict {
    node_id: String,
    verdict: String,
    #[serde(default)]
    weight: Option<f64>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct RawCorrection {
    wrong: String,
    right: String,
    #[serde(default)]
    who: Option<String>,
}

#[derive(Deserialize)]
struct RawDecision {
    decided: String,
    why: String,
    #[serde(default)]
    rejected: Option<String>,
}

#[derive(Deserialize)]
struct RawTerm {
    term: String,
    definition: String,
}

// ── Targeted Re-examination (WS-2B) ────────────────────────────────────────

/// Raw LLM response for targeted extraction.
#[derive(Deserialize)]
struct RawTargetedExtraction {
    #[serde(default)]
    extractions: Vec<RawTargetedEntry>,
}

#[derive(Deserialize)]
struct RawTargetedEntry {
    headline: String,
    distilled: String,
    #[serde(default)]
    topics: Vec<RawTopic>,
}

/// Re-examine source files through the lens of a specific gap.
///
/// Loads the targeted_extract.md prompt and calls the LLM for each source file
/// to extract evidence specifically relevant to the question and gap. Returns
/// new L0 PyramidNodes with non-empty self_prompt (targeted evidence).
pub async fn targeted_reexamination(
    question_text: &str,
    gap_description: &str,
    source_candidates: &[(String, String)], // (file_path, content)
    llm_config: &LlmConfig,
    target_slug: &str,
    build_id: &str,
    audience: Option<&str>,
    chains_dir: Option<&PathBuf>,
    ops: &OperationalConfig,
    audit: Option<&AuditContext>,
) -> Result<Vec<PyramidNode>> {
    if source_candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Draft IDs are intentionally not final node IDs. The DB writer allocates
    // canonical L0-TNNN IDs inside the serialized commit so parallel/file-local
    // calls cannot collide.
    let mut targeted_node_counter: usize = 0;

    // ── Build template variables ────────────────────────────────────────
    let audience_block = match audience {
        Some(aud) if !aud.is_empty() => format!(
            "You are writing for {aud}. Translate technical evidence into plain language.\n\n"
        ),
        _ => String::new(),
    };

    let content_type_block = String::new(); // targeted extraction is content-type agnostic

    // ── Load prompt template ────────────────────────────────────────────
    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/targeted_extract.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("audience_block", &audience_block),
                ("question_text", question_text),
                ("gap_description", gap_description),
                ("content_type_block", &content_type_block),
            ],
        ),
        None => {
            warn!("targeted_extract.md not found — using inline fallback");
            format!(
                r#"{audience_block}You are performing a TARGETED re-examination of a source file. This file was already extracted generically, but a specific question needed evidence that the generic extraction didn't capture.

THE QUESTION: {question_text}

WHAT WAS MISSING: {gap_description}

Your job: read this source file through the lens of the question above. Extract ONLY information relevant to answering that question. Do not repeat what a generic extraction would capture — focus on the specific evidence the question needs.

Be precise and specific. Names, values, relationships, mechanisms. Not summaries or overviews.

Respond with ONLY a JSON object:
{{{{
  "extractions": [
    {{{{
      "headline": "short headline describing this piece of evidence",
      "distilled": "detailed extraction — the specific evidence relevant to the question",
      "topics": [
        {{{{"name": "topic_name", "current": "what this extraction reveals about this topic"}}}}
      ]
    }}}}
  ]
}}}}"#
            )
        }
    };

    // ── Process each source file ────────────────────────────────────────
    let mut all_nodes = Vec::new();

    for (file_path, content) in source_candidates {
        let user_prompt = format!("SOURCE FILE: {}\n\n{}", file_path, content);

        // walker-v3-completion: canonical dispatch via Decision spine.
        let target_ctx = make_step_ctx_from_llm_config(
            llm_config,
            "targeted_reexamination",
            "evidence_answer",
            0,
            None,
            &system_prompt,
            "evidence_loop",
            None,
            None,
        )
        .await;
        let target_audit_ctx = audit.map(|ctx| AuditContext {
            call_purpose: "gap_answer".to_string(),
            step_name: "targeted_reexamination".to_string(),
            ..ctx.clone()
        });
        let response = match llm::call_model_unified_with_audit_and_ctx(
            llm_config,
            target_ctx.as_ref(),
            target_audit_ctx.as_ref(),
            &system_prompt,
            &user_prompt,
            ops.tier1.answer_temperature,
            ops.tier1.answer_max_tokens,
            None,
            llm::LlmCallOptions::default(),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                return Err(anyhow!(
                    "targeted extraction LLM call failed for {file_path}: {e}"
                ));
            }
        };

        info!(
            file_path = %file_path,
            tokens_in = response.usage.prompt_tokens,
            tokens_out = response.usage.completion_tokens,
            "targeted extraction LLM call complete"
        );

        // ── Parse response ──────────────────────────────────────────────
        let json_value = match llm::extract_json(&response.content) {
            Ok(v) => v,
            Err(e) => {
                return Err(anyhow!(
                    "targeted extraction JSON parse failed for {file_path}: {e}"
                ));
            }
        };

        let raw: RawTargetedExtraction = match serde_json::from_value(json_value) {
            Ok(r) => r,
            Err(e) => {
                return Err(anyhow!(
                    "targeted extraction deserialization failed for {file_path}: {e}"
                ));
            }
        };

        // ── Create PyramidNodes for each extraction ─────────────────────
        for entry in raw.extractions {
            let topics = entry
                .topics
                .into_iter()
                .map(|t| super::types::Topic {
                    name: t.name,
                    current: t.current,
                    entities: Vec::new(),
                    corrections: Vec::new(),
                    decisions: Vec::new(),
                    extra: serde_json::Map::new(),
                })
                .collect();

            let node = PyramidNode {
                id: format!("__draft-targeted-{:03}", targeted_node_counter),
                slug: target_slug.to_string(),
                depth: 0,
                chunk_index: None,
                headline: entry.headline,
                distilled: entry.distilled,
                topics,
                corrections: Vec::new(),
                decisions: Vec::new(),
                terms: Vec::new(),
                dead_ends: Vec::new(),
                self_prompt: question_text.to_string(), // MUST be non-empty
                children: Vec::new(),
                parent_id: None,
                superseded_by: None,
                build_id: Some(build_id.to_string()),
                created_at: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            };

            all_nodes.push(node);
            targeted_node_counter += 1;
        }
    }

    info!(
        question = %question_text,
        gap = %gap_description,
        source_files = source_candidates.len(),
        new_nodes = all_nodes.len(),
        "targeted re-examination complete"
    );

    Ok(all_nodes)
}

// ── Gap File Resolution (WS-2B) ────────────────────────────────────────────

/// Resolve source files that might contain evidence for a gap.
///
/// Rule-based (NO LLM): tokenizes the gap description into keywords, scores
/// canonical L0 nodes by keyword overlap, then looks up the top-scoring nodes'
/// source file paths via pyramid_file_hashes.
///
/// Returns (base_slug, file_path, content) triples.
pub fn resolve_files_for_gap(
    conn: &rusqlite::Connection,
    base_slugs: &[String],
    gap_description: &str,
    _existing_l0_nodes: &[PyramidNode],
    max_files: usize,
) -> Result<Vec<(String, String, String)>> {
    // ── 1. Tokenize gap description into keywords ───────────────────────
    // Split on non-alphanumeric boundaries (not just whitespace) to handle
    // hyphenated words like "foreign-key" → ["foreign", "key"]
    let keywords: Vec<String> = gap_description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(String::from)
        .collect();

    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    let mut scored_nodes: Vec<(String, String, usize)> = Vec::new(); // (slug, node_id, score)

    // ── 2. For each base slug, get canonical L0 nodes ──
    // Canonical L0 nodes are from the original extraction (C-L0-*, D-L0-*, or short index IDs).
    // Targeted evidence L0 nodes (from gap re-examination) use transaction-scoped
    // L0-TNNN IDs (or legacy L0-{uuid} IDs on older pyramids).
    // We include ALL L0 that are NOT targeted evidence — self_prompt is NOT a reliable
    // discriminator because canonical nodes also have self_prompt populated (orientation text).
    for base_slug in base_slugs {
        let all_l0 = db::get_nodes_at_depth(conn, base_slug, 0)?;
        let canonical: Vec<&PyramidNode> = all_l0
            .iter()
            .filter(|n| !n.id.starts_with("ES-") && !is_targeted_l0_id(&n.id))
            .collect();

        info!(
            base_slug,
            all_l0_count = all_l0.len(),
            canonical_count = canonical.len(),
            keywords_count = keywords.len(),
            first_keywords = %keywords.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            first_canonical_headline = %canonical.first().map(|n| n.headline.as_str()).unwrap_or("(none)"),
            "gap file resolution: scanning canonical L0 nodes"
        );

        // ── 3. Score each by keyword overlap (headline + distilled + topics) ──
        for node in &canonical {
            let topics_text = node
                .topics
                .iter()
                .map(|t| format!("{} {}", t.name, t.current))
                .collect::<Vec<_>>()
                .join(" ");
            let text =
                format!("{} {} {}", node.headline, node.distilled, topics_text).to_lowercase();
            let score = keywords
                .iter()
                .filter(|kw| text.contains(kw.as_str()))
                .count();
            if score > 0 {
                scored_nodes.push((base_slug.clone(), node.id.clone(), score));
            }
        }
    }

    // ── 4. Sort by score descending, take top N ────────────────────────
    scored_nodes.sort_by(|a, b| b.2.cmp(&a.2));
    scored_nodes.truncate(max_files);

    // ── 5. Look up file paths and read content ─────────────────────────
    let mut results = Vec::new();

    for (slug, node_id, _score) in &scored_nodes {
        // Find file_path from pyramid_file_hashes where node_ids contains this node
        let mut stmt = conn.prepare(
            "SELECT file_path FROM pyramid_file_hashes
             WHERE slug = ?1 AND EXISTS (SELECT 1 FROM json_each(node_ids) WHERE value = ?2)
             LIMIT 1",
        )?;
        let file_path: Option<String> = stmt
            .query_row(rusqlite::params![slug, node_id], |row| row.get(0))
            .ok();

        if let Some(path) = file_path {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    results.push((slug.clone(), path, content));
                }
                Err(e) => {
                    return Err(anyhow!(
                        "failed to read source file for gap resolution {path}: {e}"
                    ));
                }
            }
        }
    }

    info!(
        gap = %gap_description,
        keywords = keywords.len(),
        candidates_scored = scored_nodes.len(),
        files_resolved = results.len(),
        "gap file resolution complete"
    );

    Ok(results)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_raw_answer_response() {
        let json = r#"{
            "headline": "Auth flow overview",
            "distilled": "The system uses JWT tokens with refresh rotation.",
            "topics": [
                {"name": "auth", "current": "JWT-based with refresh tokens"}
            ],
            "verdicts": [
                {"node_id": "node-1", "verdict": "KEEP", "weight": 0.9, "reason": "Core auth implementation"},
                {"node_id": "node-2", "verdict": "DISCONNECT", "reason": "Unrelated to auth"},
                {"node_id": "node-3", "verdict": "KEEP", "weight": 0.4, "reason": "Tangential error handling"}
            ],
            "missing": ["OAuth2 provider configuration details"],
            "corrections": [],
            "decisions": [{"decided": "Use JWT", "why": "Stateless", "rejected": "Sessions"}],
            "terms": [{"term": "JWT", "definition": "JSON Web Token"}],
            "dead_ends": []
        }"#;

        let raw: RawAnswerResponse = serde_json::from_str(json).unwrap();
        assert_eq!(raw.headline, "Auth flow overview");
        assert_eq!(raw.verdicts.len(), 3);
        assert_eq!(raw.verdicts[0].verdict, "KEEP");
        assert_eq!(raw.verdicts[0].weight, Some(0.9));
        assert_eq!(raw.verdicts[1].verdict, "DISCONNECT");
        assert_eq!(raw.missing.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn parse_raw_answer_response_minimal() {
        let json = r#"{
            "headline": "Minimal answer",
            "distilled": "Not much evidence available.",
            "topics": [],
            "verdicts": [],
            "missing": ["Everything"]
        }"#;

        let raw: RawAnswerResponse = serde_json::from_str(json).unwrap();
        assert_eq!(raw.headline, "Minimal answer");
        assert!(raw.verdicts.is_empty());
    }

    #[test]
    fn parse_pre_map_response() {
        let json = r#"{"mappings": {"q1": ["n1", "n2"], "q2": ["n3"]}}"#;
        let raw: PreMapResponse = serde_json::from_str(json).unwrap();
        assert_eq!(raw.mappings.len(), 2);
        assert_eq!(raw.mappings["q1"].len(), 2);
    }

    #[test]
    fn targeted_l0_id_detector_handles_transaction_scoped_ids() {
        assert!(is_targeted_l0_id("L0-T000"));
        assert!(is_targeted_l0_id("L0-T042"));
        assert!(is_targeted_l0_id("L0-491a10ef-4b59-401e-9b88-8fa1bc9d0f88"));
        assert!(!is_targeted_l0_id("L0-000"));
        assert!(!is_targeted_l0_id("C-L0-000"));
        assert!(!is_targeted_l0_id("L0-TOMB000"));
    }

    #[test]
    fn resolve_files_for_gap_surfaces_source_read_failures() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        crate::pyramid::db::create_slug(
            &conn,
            "gap-io",
            &crate::pyramid::types::ContentType::Code,
            "/tmp/gap-io",
        )
        .unwrap();

        let canonical = PyramidNode {
            id: "L0-000".to_string(),
            slug: "gap-io".to_string(),
            depth: 0,
            chunk_index: None,
            headline: "missing targeted evidence".to_string(),
            distilled: "This canonical node mentions the missing targeted evidence.".to_string(),
            topics: Vec::new(),
            corrections: Vec::new(),
            decisions: Vec::new(),
            terms: Vec::new(),
            dead_ends: Vec::new(),
            self_prompt: "orientation text".to_string(),
            children: Vec::new(),
            parent_id: None,
            superseded_by: None,
            build_id: Some("build-gap-io".to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        };
        crate::pyramid::db::save_node(&conn, &canonical, None, None, ProvenanceKind::Manual)
            .unwrap();

        let tmp = tempfile::TempDir::new().unwrap();
        let missing_path = tmp.path().join("missing-source.rs");
        crate::pyramid::db::append_node_id_to_file_hash(
            &conn,
            "gap-io",
            &missing_path.to_string_lossy(),
            "L0-000",
        )
        .unwrap();

        let err = resolve_files_for_gap(
            &conn,
            &["gap-io".to_string()],
            "missing targeted evidence",
            &[],
            1,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("failed to read source file"),
            "expected source-read failure, got {err:#}"
        );
    }

    #[tokio::test]
    async fn evidence_pre_map_ctx_uses_walker_fallback_without_legacy_tier() {
        let temp_db = tempfile::NamedTempFile::new().expect("temp db");
        let conn = rusqlite::Connection::open(temp_db.path()).expect("open temp db");
        conn.execute_batch(
            "CREATE TABLE pyramid_config_contributions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 contribution_id TEXT NOT NULL UNIQUE,
                 slug TEXT,
                 schema_type TEXT NOT NULL,
                 yaml_content TEXT NOT NULL,
                 wire_native_metadata_json TEXT NOT NULL DEFAULT '{}',
                 wire_publication_state_json TEXT NOT NULL DEFAULT '{}',
                 supersedes_id TEXT,
                 superseded_by_id TEXT,
                 triggering_note TEXT,
                 status TEXT NOT NULL DEFAULT 'active',
                 source TEXT NOT NULL DEFAULT 'local',
                 wire_contribution_id TEXT,
                 created_by TEXT,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 accepted_at TEXT
             );",
        )
        .expect("create contributions table");
        conn.execute(
            "INSERT INTO pyramid_config_contributions
                 (contribution_id, schema_type, yaml_content, status, accepted_at)
             VALUES (?1, ?2, ?3, 'active', datetime('now'))",
            rusqlite::params![
                "fallback-only-openrouter",
                "walker_provider_openrouter",
                "schema_type: walker_provider_openrouter\nversion: 1\noverrides:\n  model_list:\n    fallback:\n      - \"fallback/evidence-model\"\n"
            ],
        )
        .expect("insert fallback provider");

        let config = crate::pyramid::llm::LlmConfig::default().clone_with_cache_access(
            "evidence-pre-map-fallback",
            "build-evidence-pre-map-fallback",
            temp_db.path().to_string_lossy().to_string(),
            None,
        );

        let ctx = make_step_ctx_from_llm_config(
            &config,
            "evidence_pre_map_0",
            "evidence_pre_map",
            0,
            Some(0),
            "system prompt",
            "evidence_loop",
            None,
            None,
        )
        .await
        .expect("evidence pre-map ctx");

        assert_eq!(ctx.model_tier, "evidence_loop");
        assert_eq!(ctx.resolved_model_id.as_deref(), Some("fallback/evidence-model"));
        assert!(
            ctx.dispatch_decision.is_some(),
            "evidence pre-map should not require legacy pyramid_tier_routing"
        );
    }

    #[test]
    fn verdict_weight_clamping() {
        // Verify weight clamping logic
        let weight = Some(1.5_f64);
        let clamped = weight.unwrap_or(0.5).clamp(0.0, 1.0);
        assert_eq!(clamped, 1.0);

        let weight_neg = Some(-0.3_f64);
        let clamped_neg = weight_neg.unwrap_or(0.5).clamp(0.0, 1.0);
        assert_eq!(clamped_neg, 0.0);

        let weight_none: Option<f64> = None;
        let clamped_none = weight_none.unwrap_or(0.5).clamp(0.0, 1.0);
        assert_eq!(clamped_none, 0.5);
    }
}

// ── Phase 12 Triage Gate ──────────────────────────────────────────────────

/// Statistics returned by the triage gate.
#[derive(Debug, Clone, Default)]
pub struct TriageStats {
    pub evaluated: usize,
    pub answered: usize,
    pub deferred: usize,
    pub skipped: usize,
}

/// Per-question triage decision record captured for Phase 13
/// observability. The tuple is `(question_id, decision_tag, reason)`
/// where `decision_tag` is one of `"answer"`, `"defer"`, `"skip"`.
/// Reason carries the matched rule description or a default tag.
#[derive(Debug, Clone)]
pub struct TriageDecisionRecord {
    pub question_id: String,
    pub decision_tag: String,
    pub reason: String,
}

/// Result of running the triage gate over a batch of questions.
pub struct TriageGateResult {
    pub answer_questions: Vec<LayerQuestion>,
    pub stats: TriageStats,
    /// Phase 13: per-question decisions in the order they were made,
    /// so the caller can emit `TriageDecision` events on the bus.
    pub decisions: Vec<TriageDecisionRecord>,
}

/// Phase 12: Open a connection, load the active evidence_policy,
/// evaluate each question through the triage DSL, and partition
/// questions into three buckets. The Answer bucket is returned to
/// the caller; Defer questions are persisted to
/// `pyramid_deferred_questions`; Skip questions are dropped and
/// logged.
///
/// This runs inside `spawn_blocking` — no async code, no LLM calls
/// (triage LLM classification is Phase 13 scope per the workstream
/// prompt's most-load-bearing-reasonable-call guidance).
pub fn run_triage_gate(
    db_path: &str,
    slug: &str,
    questions: &[LayerQuestion],
) -> Result<TriageGateResult> {
    use super::triage::{resolve_decision, TriageDecision};
    use std::path::Path;

    let mut stats = TriageStats {
        evaluated: questions.len(),
        ..Default::default()
    };

    // Load policy.
    let conn = match db::open_pyramid_connection(Path::new(db_path)) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                error = %e,
                "triage gate: failed to open DB connection, falling back to answer-all"
            );
            stats.answered = questions.len();
            let decisions = questions
                .iter()
                .map(|q| TriageDecisionRecord {
                    question_id: q.question_id.clone(),
                    decision_tag: "answer".into(),
                    reason: "no_policy_conn".into(),
                })
                .collect();
            return Ok(TriageGateResult {
                answer_questions: questions.to_vec(),
                stats,
                decisions,
            });
        }
    };

    let policy = match db::load_active_evidence_policy(&conn, Some(slug)) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                error = %e,
                "triage gate: failed to load evidence policy, falling back to answer-all"
            );
            stats.answered = questions.len();
            let decisions = questions
                .iter()
                .map(|q| TriageDecisionRecord {
                    question_id: q.question_id.clone(),
                    decision_tag: "answer".into(),
                    reason: "policy_load_failed".into(),
                })
                .collect();
            return Ok(TriageGateResult {
                answer_questions: questions.to_vec(),
                stats,
                decisions,
            });
        }
    };

    // If no rules are configured, skip the gate entirely.
    if policy.triage_rules.is_empty() {
        stats.answered = questions.len();
        let decisions = questions
            .iter()
            .map(|q| TriageDecisionRecord {
                question_id: q.question_id.clone(),
                decision_tag: "answer".into(),
                reason: "no_rules".into(),
            })
            .collect();
        return Ok(TriageGateResult {
            answer_questions: questions.to_vec(),
            stats,
            decisions,
        });
    }

    // Phase 12 verifier fix: compute `is_first_build` from DB state
    // once per-call so the spec's canonical "first_build AND depth == 0"
    // rule matches on fresh builds. `is_first_build == true` when no
    // L0 nodes exist for this slug yet.
    let is_first_build = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_nodes WHERE slug = ?1 AND depth = 0",
            rusqlite::params![slug],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| c == 0)
        .unwrap_or(false);

    let mut answer_bucket: Vec<LayerQuestion> = Vec::new();
    let mut decisions: Vec<TriageDecisionRecord> = Vec::with_capacity(questions.len());

    // Phase 12 wanderer fix: `has_demand_signals` is evaluated
    // ONCE per triage pass, at slug granularity, not per-question.
    //
    // The per-node `sum_demand_weight(slug, node_id, ...)` can't be
    // used here because `LayerQuestion.question_id` is a question handle
    // like `Q-L1-000`, while demand signals land on
    // `pyramid_demand_signals.node_id` under the answered pyramid node's
    // `L{layer}-{seq}` id. The two ID spaces never meet, so
    // the previous per-question lookup always returned 0.0 and
    // `has_demand_signals` was effectively dead.
    //
    // Per-slug aggregation matches the spec's intent ("drive re-check
    // by demand") while staying correct in the only ID space the
    // demand signals actually live in. The spatial precision the
    // spec implies will come back in Phase 13+ when a persistent
    // question-handle → answer-node map is added.
    let slug_has_demand_signals = policy.demand_signals.iter().any(|rule| {
        let window = normalize_window(&rule.window);
        let sum = db::sum_slug_demand_weight(&conn, slug, &rule.r#type, &window).unwrap_or(0.0);
        sum >= rule.threshold
    });

    for question in questions {
        let has_demand_signals = slug_has_demand_signals;

        let facts = super::triage::TriageFacts {
            question,
            target_node_distilled: None,
            target_node_depth: Some(question.layer),
            is_first_build,
            is_stale_check: false,
            has_demand_signals,
            evidence_question_trivial: None,
            evidence_question_high_value: None,
        };

        let decision = match resolve_decision(&policy, &facts) {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    question_id = %question.question_id,
                    error = %e,
                    "triage DSL evaluation failed; defaulting to Answer"
                );
                TriageDecision::Answer {
                    model_tier: "evidence_loop".to_string(),
                }
            }
        };

        match decision {
            TriageDecision::Answer { ref model_tier } => {
                decisions.push(TriageDecisionRecord {
                    question_id: question.question_id.clone(),
                    decision_tag: "answer".to_string(),
                    reason: format!("model_tier={}", model_tier),
                });
                answer_bucket.push(question.clone());
                stats.answered += 1;
            }
            TriageDecision::Defer {
                ref check_interval,
                ref triage_reason,
            } => {
                let check_interval = check_interval.clone();
                let triage_reason = triage_reason.clone();
                let qjson = serde_json::to_string(question).unwrap_or_else(|_| "{}".into());
                let contribution_id = policy.contribution_id.as_deref();
                if let Err(e) = db::defer_question(
                    &conn,
                    slug,
                    &question.question_id,
                    &qjson,
                    &check_interval,
                    Some(&triage_reason),
                    contribution_id,
                ) {
                    warn!(
                        question_id = %question.question_id,
                        error = %e,
                        "triage defer: failed to persist deferred question, answering instead"
                    );
                    decisions.push(TriageDecisionRecord {
                        question_id: question.question_id.clone(),
                        decision_tag: "answer".to_string(),
                        reason: format!("defer_persist_failed: {}", e),
                    });
                    answer_bucket.push(question.clone());
                    stats.answered += 1;
                } else {
                    decisions.push(TriageDecisionRecord {
                        question_id: question.question_id.clone(),
                        decision_tag: "defer".to_string(),
                        reason: triage_reason,
                    });
                    stats.deferred += 1;
                }
            }
            TriageDecision::Skip { ref reason } => {
                let reason = reason.clone();
                info!(
                    question_id = %question.question_id,
                    reason = %reason,
                    "triage skip: dropping question"
                );
                decisions.push(TriageDecisionRecord {
                    question_id: question.question_id.clone(),
                    decision_tag: "skip".to_string(),
                    reason,
                });
                stats.skipped += 1;
            }
        }
    }

    Ok(TriageGateResult {
        answer_questions: answer_bucket,
        stats,
        decisions,
    })
}

/// Normalize a window string to a SQLite datetime modifier format.
/// Accepts both "7d"/"14d" and "-7 days"/"-14 days" styles.
fn normalize_window(window: &str) -> String {
    let w = window.trim();
    if w.starts_with('-') || w.contains(' ') {
        return w.to_string();
    }
    // Short form: "7d" → "-7 days"; "14d" → "-14 days"; "1h" → "-1 hours".
    let (num_part, unit_part): (String, String) = w.chars().partition(|c| c.is_ascii_digit());
    let n: i64 = num_part.parse().unwrap_or(14);
    let unit = match unit_part.as_str() {
        "d" => "days",
        "h" => "hours",
        "w" => "days",
        "m" => "minutes",
        _ => "days",
    };
    let n = if unit == "days" && unit_part == "w" {
        n * 7
    } else {
        n
    };
    format!("-{} {}", n, unit)
}

// ── Phase 12 Triage Gate Tests ────────────────────────────────────────────

#[cfg(test)]
mod triage_gate_tests {
    use super::*;
    use crate::pyramid::db::{
        init_pyramid_db, upsert_evidence_policy, DemandSignalRuleYaml, EvidencePolicyYaml,
        PolicyBudgetYaml, TriageRuleYaml,
    };
    use rusqlite::Connection;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    fn mk_question(id: &str) -> LayerQuestion {
        LayerQuestion {
            question_id: id.to_string(),
            question_text: "?".to_string(),
            layer: 1,
            about: "".to_string(),
            creates: "".to_string(),
        }
    }

    #[test]
    fn test_triage_gate_fallthrough_when_no_policy() {
        let conn = mem_conn();
        // No policy → all questions go to Answer bucket.
        // Persist the DB to a file so run_triage_gate can reopen it.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_string_lossy().to_string();
        let real_conn = crate::pyramid::db::open_pyramid_connection(tmp.path()).unwrap();
        init_pyramid_db(&real_conn).unwrap();

        let questions = vec![mk_question("Q1"), mk_question("Q2")];
        let result = run_triage_gate(&db_path, "no-policy-slug", &questions).unwrap();
        assert_eq!(result.answer_questions.len(), 2);
        assert_eq!(result.stats.answered, 2);
        assert_eq!(result.stats.deferred, 0);
        assert_eq!(result.stats.skipped, 0);
        let _ = conn;
    }

    #[test]
    fn test_triage_gate_partitions_questions() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_string_lossy().to_string();
        let conn = crate::pyramid::db::open_pyramid_connection(tmp.path()).unwrap();
        init_pyramid_db(&conn).unwrap();

        // Register a contribution row for FK.
        conn.execute(
            "INSERT INTO pyramid_config_contributions
                (contribution_id, slug, schema_type, yaml_content, status)
             VALUES ('c-pol', 'part-slug', 'evidence_policy', '', 'active')",
            [],
        )
        .unwrap();

        // Policy: defer all questions at layer 1 (the default in mk_question).
        let yaml = EvidencePolicyYaml {
            triage_rules: Some(vec![TriageRuleYaml {
                condition: "depth == 1".into(),
                action: "defer".into(),
                check_interval: Some("7d".into()),
                ..Default::default()
            }]),
            demand_signals: None,
            budget: Some(PolicyBudgetYaml::default()),
            demand_signal_attenuation: None,
        };
        upsert_evidence_policy(&conn, &Some("part-slug".to_string()), &yaml, "c-pol").unwrap();
        drop(conn);

        let questions = vec![mk_question("Q1"), mk_question("Q2"), mk_question("Q3")];
        let result = run_triage_gate(&db_path, "part-slug", &questions).unwrap();
        assert_eq!(result.answer_questions.len(), 0);
        assert_eq!(result.stats.answered, 0);
        assert_eq!(result.stats.deferred, 3);
        assert_eq!(result.stats.skipped, 0);

        // Verify deferred rows landed.
        let reopen = crate::pyramid::db::open_pyramid_connection(tmp.path()).unwrap();
        let all = crate::pyramid::db::list_all_deferred(&reopen, "part-slug").unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_normalize_window_short_and_long_forms() {
        assert_eq!(normalize_window("7d"), "-7 days");
        assert_eq!(normalize_window("14d"), "-14 days");
        assert_eq!(normalize_window("-7 days"), "-7 days");
        assert_eq!(normalize_window("1h"), "-1 hours");
        assert_eq!(normalize_window("2w"), "-14 days");
    }
}
