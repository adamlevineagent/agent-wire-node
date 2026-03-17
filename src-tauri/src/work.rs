// Wire Node — Mechanical Work Engine
//
// Polls for work from the Wire API, executes it locally, submits results.
// Work types: cache_item, verify_item, grade_contribution, enrich_item, extract_source_document
// Each completed job earns credits for the node operator.

use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};

/// A work item received from the server
#[derive(Debug, Clone, Deserialize)]
pub struct WorkItem {
    pub id: String,
    #[serde(rename = "type")]
    pub work_type: String,
    pub payload: serde_json::Value,
}

/// Result of executing a work item
#[derive(Debug, Clone, Serialize)]
pub struct WorkResult {
    pub success: bool,
    pub data: serde_json::Value,
}

/// Response from submitting a work result
#[derive(Debug, Deserialize)]
pub struct SubmitResponse {
    pub credits_awarded: f64,
    #[serde(default)]
    pub balance_after: f64,
}

/// Work engine stats
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WorkStats {
    pub total_jobs_completed: u64,
    pub total_credits_earned: f64,
    pub session_jobs_completed: u64,
    pub session_credits_earned: f64,
    pub consecutive_errors: u32,
    pub last_work_at: Option<String>,
    pub is_polling: bool,
}

// --- Work Polling ---

/// Poll for available work from the Wire API
pub async fn poll_work(
    api_url: &str,
    api_token: &str,
    node_id: &str,
) -> Result<Option<WorkItem>, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/node/work?node_id={}", api_url, node_id);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Work poll failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Work poll returned {}: {}", status, text));
    }

    let body: serde_json::Value = resp.json().await
        .map_err(|e| format!("Work poll parse error: {}", e))?;

    // The response has { work: null } when no work available
    if let Some(work_value) = body.get("work") {
        if work_value.is_null() {
            return Ok(None);
        }
        let work: WorkItem = serde_json::from_value(work_value.clone())
            .map_err(|e| format!("Work item parse error: {}", e))?;
        return Ok(Some(work));
    }

    Ok(None)
}

/// Submit a work result to the Wire API
pub async fn submit_result(
    api_url: &str,
    api_token: &str,
    work_id: &str,
    result: &serde_json::Value,
) -> Result<SubmitResponse, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/node/result", api_url);

    let body = serde_json::json!({
        "work_id": work_id,
        "result": result,
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Result submit failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Result submit returned {}: {}", status, text));
    }

    let submit_resp: SubmitResponse = resp.json().await
        .map_err(|e| format!("Result response parse error: {}", e))?;

    Ok(submit_resp)
}

// --- Work Execution ---

/// Execute a work item based on its type
pub async fn execute_work(work: &WorkItem) -> WorkResult {
    match work.work_type.as_str() {
        "cache_item" => execute_cache_item(&work.payload).await,
        "verify_item" => execute_verify_item(&work.payload),
        "grade_contribution" => execute_grade_contribution(&work.payload),
        "enrich_item" => execute_enrich_item(&work.payload).await,
        "extract_source_document" => execute_extract_source_document(&work.payload),
        _ => WorkResult {
            success: false,
            data: serde_json::json!({ "error": format!("Unknown work type: {}", work.work_type) }),
        },
    }
}

/// cache_item: HEAD request to URL, check accessibility
async fn execute_cache_item(payload: &serde_json::Value) -> WorkResult {
    let url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let item_id = payload.get("item_id").and_then(|v| v.as_str()).unwrap_or("");

    if url.is_empty() {
        return WorkResult {
            success: false,
            data: serde_json::json!({ "error": "No URL provided for cache item", "item_id": item_id }),
        };
    }

    let client = reqwest::Client::new();
    match client
        .head(url)
        .header("User-Agent", "WireNode/0.2 (+https://agent-wire.com)")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(res) => {
            let status = res.status().as_u16();
            let content_type = res.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
            let content_length: Option<u64> = res.headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());

            WorkResult {
                success: true,
                data: serde_json::json!({
                    "item_id": item_id,
                    "url": url,
                    "status": status,
                    "content_type": content_type,
                    "content_length": content_length,
                    "accessible": res.status().is_success(),
                    "checked_at": chrono::Utc::now().to_rfc3339(),
                }),
            }
        }
        Err(e) => WorkResult {
            success: false,
            data: serde_json::json!({
                "error": format!("Cache check failed: {}", e),
                "item_id": item_id,
                "url": url,
            }),
        },
    }
}

/// verify_item: SHA-256 hash verification
fn execute_verify_item(payload: &serde_json::Value) -> WorkResult {
    let item_id = payload.get("item_id").and_then(|v| v.as_str()).unwrap_or("");
    let expected_hash = payload.get("content_hash").and_then(|v| v.as_str()).unwrap_or("");
    let content = payload.get("content").and_then(|v| v.as_str()).unwrap_or("");

    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let actual_hash = hex::encode(hasher.finalize());

    if expected_hash.is_empty() {
        return WorkResult {
            success: true,
            data: serde_json::json!({
                "item_id": item_id,
                "verified": false,
                "reason": "no_expected_hash",
                "actual_hash": actual_hash,
            }),
        };
    }

    let matches = actual_hash == expected_hash;

    WorkResult {
        success: true,
        data: serde_json::json!({
            "item_id": item_id,
            "verified": matches,
            "actual_hash": actual_hash,
            "expected_hash": expected_hash,
        }),
    }
}

/// grade_contribution: Heuristic quality scoring
fn execute_grade_contribution(payload: &serde_json::Value) -> WorkResult {
    let title = payload.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let entities = payload.get("entities")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let word_count = body.split_whitespace().count();
    let has_entities = entities > 0;
    let _has_structure = body.contains('\n') || body.len() > 200;

    // Simple quality heuristics (matches npm implementation)
    let accuracy = f64::min(
        1.0,
        (if word_count > 50 { 0.5 } else { 0.3 })
            + (if has_entities { 0.3 } else { 0.0 })
            + (if title.len() > 10 { 0.2 } else { 0.0 }),
    );
    let completeness = f64::min(1.0, word_count as f64 / 500.0);
    let usefulness = (accuracy + completeness) / 2.0;

    let pass = accuracy >= 0.5 && completeness >= 0.5 && usefulness >= 0.5;

    // Round to 2 decimal places
    let accuracy = (accuracy * 100.0).round() / 100.0;
    let completeness = (completeness * 100.0).round() / 100.0;
    let usefulness = (usefulness * 100.0).round() / 100.0;

    WorkResult {
        success: true,
        data: serde_json::json!({
            "accuracy": accuracy,
            "completeness": completeness,
            "usefulness": usefulness,
            "pass": pass,
        }),
    }
}

/// enrich_item: Fetch URL, extract text, extract entities
async fn execute_enrich_item(payload: &serde_json::Value) -> WorkResult {
    let url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let expected_title = payload.get("expected_title").and_then(|v| v.as_str()).unwrap_or("");

    // If no URL, fall back to entity extraction on content snippet
    if url.is_empty() {
        let content = payload.get("content_snippet")
            .or_else(|| payload.get("content"))
            .or_else(|| payload.get("body"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let entities = extract_entities(content);
        return WorkResult {
            success: true,
            data: serde_json::json!({
                "entities": entities,
                "entity_count": entities.len(),
            }),
        };
    }

    let client = reqwest::Client::new();
    match client
        .get(url)
        .header("User-Agent", "WireNode/0.2 (+https://agent-wire.com)")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(res) => {
            if !res.status().is_success() {
                return WorkResult {
                    success: false,
                    data: serde_json::json!({
                        "error": format!("Fetch failed: {}", res.status()),
                        "url": url,
                    }),
                };
            }

            match res.text().await {
                Ok(html) => {
                    // Basic text extraction: strip HTML tags
                    let text = strip_html(&html);
                    if text.is_empty() {
                        return WorkResult {
                            success: false,
                            data: serde_json::json!({
                                "error": "Text extraction returned no content",
                                "url": url,
                            }),
                        };
                    }

                    // Extract title from <title> tag
                    let article_title = extract_html_title(&html).unwrap_or_default();

                    // Fuzzy title match
                    let title_match = if !expected_title.is_empty() && !article_title.is_empty() {
                        let et = expected_title.to_lowercase();
                        let at = article_title.to_lowercase();
                        at.contains(&et) || et.contains(&at)
                    } else {
                        false
                    };

                    // SHA-256 hash
                    let mut hasher = Sha256::new();
                    hasher.update(text.as_bytes());
                    let full_text_hash = hex::encode(hasher.finalize());

                    // Entity extraction
                    let entities = extract_entities(&text);

                    let word_count = text.split_whitespace().count();

                    WorkResult {
                        success: true,
                        data: serde_json::json!({
                            "full_text": text,
                            "full_text_hash": full_text_hash,
                            "title_match": title_match,
                            "entities": entities,
                            "word_count": word_count,
                            "fetch_timestamp": chrono::Utc::now().to_rfc3339(),
                        }),
                    }
                }
                Err(e) => WorkResult {
                    success: false,
                    data: serde_json::json!({
                        "error": format!("Body read failed: {}", e),
                        "url": url,
                    }),
                },
            }
        }
        Err(e) => WorkResult {
            success: false,
            data: serde_json::json!({
                "error": format!("Enrich failed: {}", e),
                "url": url,
            }),
        },
    }
}

/// extract_source_document: Extract text from source documents
fn execute_extract_source_document(payload: &serde_json::Value) -> WorkResult {
    let document_id = payload.get("document_id").and_then(|v| v.as_str()).unwrap_or("");
    let format = payload.get("format").and_then(|v| v.as_str()).unwrap_or("text/markdown");
    let body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");

    if document_id.is_empty() {
        return WorkResult {
            success: false,
            data: serde_json::json!({ "error": "No document_id provided", "document_id": document_id }),
        };
    }

    let extracted_text = if format == "application/pdf" {
        // PDF extraction: base64 decode → basic text extraction
        // For now, we attempt base64 decode but skip full PDF parsing
        // (would need a PDF crate — the text formats cover most use cases)
        match base64::engine::general_purpose::STANDARD.decode(body) {
            Ok(pdf_bytes) => {
                // Basic heuristic: extract printable ASCII sequences from PDF
                // This is a fallback — proper PDF parsing would use pdf-extract crate
                let text: String = pdf_bytes.iter()
                    .filter(|&&b| b >= 0x20 && b < 0x7f || b == b'\n' || b == b'\r' || b == b'\t')
                    .map(|&b| b as char)
                    .collect();
                if text.trim().is_empty() {
                    return WorkResult {
                        success: false,
                        data: serde_json::json!({
                            "error": "PDF text extraction produced no readable content",
                            "document_id": document_id,
                        }),
                    };
                }
                text
            }
            Err(e) => {
                return WorkResult {
                    success: false,
                    data: serde_json::json!({
                        "error": format!("Base64 decode failed: {}", e),
                        "document_id": document_id,
                    }),
                };
            }
        }
    } else {
        // text/plain, text/markdown, text/html: body is already text
        body.to_string()
    };

    // SHA-256 hash
    let mut hasher = Sha256::new();
    hasher.update(extracted_text.as_bytes());
    let body_hash = hex::encode(hasher.finalize());

    // Word count
    let word_count = extracted_text.split_whitespace().filter(|s| !s.is_empty()).count();

    WorkResult {
        success: true,
        data: serde_json::json!({
            "document_id": document_id,
            "extracted_text": extracted_text,
            "word_count": word_count,
            "body_hash": body_hash,
            "format": format,
        }),
    }
}

// --- Helpers ---

/// Strip HTML tags to extract plain text (basic implementation)
fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let lower = html.to_lowercase();

    let chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if !in_tag && i + 7 < len {
            let slice: String = lower_chars[i..i + 7].iter().collect();
            if slice == "<script" {
                in_script = true;
            } else if slice == "<style " || (i + 6 < len && lower_chars[i..i + 6].iter().collect::<String>() == "<style") {
                in_style = true;
            }
        }

        if chars[i] == '<' {
            in_tag = true;

            // Check for closing script/style
            if in_script && i + 9 < len {
                let slice: String = lower_chars[i..i + 9].iter().collect();
                if slice == "</script>" {
                    in_script = false;
                    i += 9;
                    in_tag = false;
                    continue;
                }
            }
            if in_style && i + 8 < len {
                let slice: String = lower_chars[i..i + 8].iter().collect();
                if slice == "</style>" {
                    in_style = false;
                    i += 8;
                    in_tag = false;
                    continue;
                }
            }

            i += 1;
            continue;
        }

        if chars[i] == '>' {
            in_tag = false;
            i += 1;
            continue;
        }

        if !in_tag && !in_script && !in_style {
            result.push(chars[i]);
        }

        i += 1;
    }

    // Decode common HTML entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    // Collapse whitespace
    let mut collapsed = String::with_capacity(result.len());
    let mut last_was_space = false;
    for c in result.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                collapsed.push(' ');
                last_was_space = true;
            }
        } else {
            collapsed.push(c);
            last_was_space = false;
        }
    }

    collapsed.trim().to_string()
}

/// Extract title from HTML <title> tag
fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title>")?;
    let end = lower[start..].find("</title>")?;
    let title = &html[start + 7..start + end];
    Some(title.trim().to_string())
}

/// Extract named entities via capitalized word sequences (matches npm implementation)
fn extract_entities(text: &str) -> Vec<serde_json::Value> {
    let mut entities = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Simple regex-like: find sequences of capitalized words
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut i = 0;

    while i < words.len() {
        if is_capitalized(words[i]) && words[i].len() > 2 {
            let mut phrase = words[i].to_string();
            let mut j = i + 1;

            // Extend to multi-word phrases
            while j < words.len() && is_capitalized(words[j]) {
                phrase.push(' ');
                phrase.push_str(words[j]);
                j += 1;
            }

            // Clean trailing punctuation
            let clean = phrase.trim_end_matches(|c: char| !c.is_alphanumeric()).to_string();

            if clean.len() > 2 && !seen.contains(&clean) {
                seen.insert(clean.clone());
                let entity_type = if clean.contains(' ') {
                    "organization"
                } else {
                    "person"
                };
                entities.push(serde_json::json!({
                    "name": clean,
                    "type": entity_type,
                }));

                if entities.len() >= 20 {
                    break;
                }
            }

            i = j;
        } else {
            i += 1;
        }
    }

    entities
}

/// Check if a word starts with a capital letter
fn is_capitalized(word: &str) -> bool {
    word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}

use base64::Engine as _; // needed for base64::engine::general_purpose::STANDARD.decode()
