use std::path::Path;

use serde_json::Value;

use super::types::{PyramidNode, Topic};

const MAX_HEADLINE_CHARS: usize = 72;
const MAX_HEADLINE_WORDS: usize = 8;

pub fn clean_headline(raw: &str) -> Option<String> {
    let collapsed = raw
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let trimmed = collapsed.trim().trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\'' | '`' | '.' | ',' | ';' | ':' | '-' | '|' | ' '
        )
    });

    if trimmed.is_empty() {
        return None;
    }

    let first_segment = trimmed
        .split(['\n', '\r'])
        .next()
        .unwrap_or(trimmed)
        .split(". ")
        .next()
        .unwrap_or(trimmed)
        .trim();

    if first_segment.is_empty() {
        return None;
    }

    let lower = first_segment.to_ascii_lowercase();
    let banned_prefixes = [
        "this node ",
        "the node ",
        "this file ",
        "the file ",
        "this module ",
        "the module ",
        "this document ",
        "the document ",
        "this summary ",
        "the summary ",
        "current state ",
        "updated distillation ",
        "orientation ",
        "summary ",
    ];
    if banned_prefixes
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return None;
    }

    let words = first_segment.split_whitespace().collect::<Vec<_>>();
    let shortened = if words.len() > MAX_HEADLINE_WORDS {
        words[..MAX_HEADLINE_WORDS].join(" ")
    } else {
        first_segment.to_string()
    };

    let char_count = shortened.chars().count();
    let final_text = if char_count > MAX_HEADLINE_CHARS {
        shortened
            .chars()
            .take(MAX_HEADLINE_CHARS)
            .collect::<String>()
    } else {
        shortened
    };

    let final_text = final_text.trim().trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\'' | '`' | '.' | ',' | ';' | ':' | '-' | '|' | ' '
        )
    });

    if final_text.is_empty() {
        None
    } else {
        Some(final_text.to_string())
    }
}

fn title_case_token(token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }

    if token.chars().all(|ch| !ch.is_ascii_lowercase()) {
        return token.to_string();
    }

    let mut chars = token.chars();
    let first = chars.next().unwrap();
    format!(
        "{}{}",
        first.to_ascii_uppercase(),
        chars.as_str().to_ascii_lowercase()
    )
}

fn humanize_identifier(input: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev: Option<char> = None;

    for ch in input.chars() {
        if matches!(ch, '_' | '-' | '.' | '/') {
            if !out.ends_with(' ') {
                out.push(' ');
            }
            prev = Some(ch);
            continue;
        }

        if let Some(prev_ch) = prev {
            if !matches!(prev_ch, '_' | '-' | '.' | '/')
                && ((prev_ch.is_ascii_lowercase() && ch.is_ascii_uppercase())
                    || (prev_ch.is_ascii_alphabetic() && ch.is_ascii_digit())
                    || (prev_ch.is_ascii_digit() && ch.is_ascii_alphabetic()))
                && !out.ends_with(' ')
            {
                out.push(' ');
            }
        }

        out.push(ch);
        prev = Some(ch);
    }

    let tokens = out
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(title_case_token)
        .collect::<Vec<_>>();

    clean_headline(&tokens.join(" "))
}

pub fn headline_from_path(path: &str) -> Option<String> {
    let path = Path::new(path);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())?;
    humanize_identifier(stem)
}

fn headline_from_topics(topics: &[Topic]) -> Option<String> {
    topics
        .iter()
        .filter_map(|topic| clean_headline(&topic.name))
        .next()
}

fn headline_from_analysis_topics(analysis: &Value) -> Option<String> {
    analysis
        .get("topics")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|topic| topic.get("name").and_then(|value| value.as_str()))
        .filter_map(clean_headline)
        .next()
}

fn headline_from_distilled(distilled: &str) -> Option<String> {
    let candidate = distilled
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(distilled);
    clean_headline(candidate)
}

pub fn headline_from_analysis(analysis: &Value, node_id: &str) -> String {
    analysis
        .get("headline")
        .and_then(|value| value.as_str())
        .and_then(clean_headline)
        .or_else(|| {
            analysis
                .get("title")
                .and_then(|value| value.as_str())
                .and_then(clean_headline)
        })
        .or_else(|| {
            analysis
                .get("name")
                .and_then(|value| value.as_str())
                .and_then(clean_headline)
        })
        .or_else(|| {
            analysis
                .get("purpose")
                .and_then(|value| value.as_str())
                .and_then(clean_headline)
        })
        .or_else(|| headline_from_analysis_topics(analysis))
        .or_else(|| {
            analysis
                .get("orientation")
                .and_then(|value| value.as_str())
                .and_then(headline_from_distilled)
        })
        .or_else(|| {
            analysis
                .get("distilled")
                .and_then(|value| value.as_str())
                .and_then(headline_from_distilled)
        })
        .unwrap_or_else(|| format!("Node {node_id}"))
}

pub fn headline_for_node(node: &PyramidNode, source_path: Option<&str>) -> String {
    clean_headline(&node.headline)
        .or_else(|| {
            if node.depth == 0 {
                source_path.and_then(headline_from_path)
            } else {
                None
            }
        })
        .or_else(|| headline_from_topics(&node.topics))
        .or_else(|| source_path.and_then(headline_from_path))
        .or_else(|| headline_from_distilled(&node.distilled))
        .unwrap_or_else(|| format!("Node {}", node.id))
}

pub fn tombstone_headline(file_path: &str) -> String {
    let base = headline_from_path(file_path).unwrap_or_else(|| "Deleted File".to_string());
    clean_headline(&format!("Deleted {base}")).unwrap_or(base)
}
