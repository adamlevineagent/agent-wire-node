//! WS-L — ASCII art generator with supersession.
//!
//! Produces per-pyramid "banner" art by calling Grok 4.2 directly (bypassing
//! the default Mercury-2 cascade, which empirically fails at ASCII art per C4).
//!
//! Storage obeys C1: `pyramid_ascii_art` is supersession-aware. Each
//! generation INSERTs a new row and sets `superseded_by` on the previous
//! head for the same `(slug, kind)`. We never UPDATE or DELETE existing rows
//! (Pillar 1, 5).
//!
//! Generation runs only on operator command or the build pipeline final step
//! (A11). A per-slug `tokio::Mutex` provides single-flight.
//!
//! Pillar 37: the prompt frames the 72-column width as a MEDIUM CONSTRAINT,
//! not a quota. Validation is post-hoc (strip blank lines, cap height/width).

use crate::pyramid::PyramidState;
use rusqlite::{params, Connection, OptionalExtension, Result as SqlResult};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

/// Grok 4.2 — pinned per C4. Mercury-2 fails at ASCII art (tested).
pub const ASCII_ART_MODEL: &str = "x-ai/grok-4.20-beta";

/// Kinds of art we may cache. V1 only uses `Banner`; the others are reserved
/// for V2 so the `kind` column has a stable vocabulary.
#[derive(Debug, Clone, Copy)]
pub enum ArtKind {
    Banner,
    TopicDivider,
    Hero,
}

impl ArtKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArtKind::Banner => "banner",
            ArtKind::TopicDivider => "topic-divider",
            ArtKind::Hero => "hero",
        }
    }
}

// ── Single-flight table keyed by slug ────────────────────────────────────────

static INFLIGHT: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn inflight_map() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Generate (or return cached) banner ASCII art for `slug`.
///
/// Flow:
///   1. Acquire single-flight lock for this slug.
///   2. Read the apex headline (the pyramid's top node, at max_depth).
///   3. Hash `apex_headline || kind || model` → `source_hash`.
///   4. If current head row already matches `source_hash`, return its text.
///   5. Build the prompt (Pillar 37 wording), call Grok 4.2 directly.
///   6. Validate post-hoc (strip blanks, cap width/height).
///   7. INSERT new row + point previous head's `superseded_by` at it.
pub async fn generate_banner_for_slug(
    state: Arc<PyramidState>,
    slug: &str,
) -> Result<String, String> {
    // 1. single-flight
    let lock = {
        let mut map = inflight_map().lock().await;
        map.entry(slug.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;

    // 2. read apex headline (highest-depth node per get_apex semantics)
    let apex_headline = {
        let conn = state.reader.lock().await;
        let slug_info = crate::pyramid::db::get_slug(&conn, slug)
            .map_err(|e| format!("get_slug: {e}"))?
            .ok_or_else(|| format!("slug not found: {slug}"))?;
        let nodes =
            crate::pyramid::db::get_nodes_at_depth(&conn, slug, slug_info.max_depth)
                .map_err(|e| format!("get_nodes_at_depth: {e}"))?;
        nodes
            .into_iter()
            .next()
            .map(|n| n.headline)
            .unwrap_or_else(|| slug.to_string())
    };

    // 3. source hash
    let source_hash = {
        let mut h = Sha256::new();
        h.update(apex_headline.as_bytes());
        h.update(b":banner:");
        h.update(ASCII_ART_MODEL.as_bytes());
        let full = format!("{:x}", h.finalize());
        full[..16].to_string()
    };

    // 4. hit check
    {
        let conn = state.reader.lock().await;
        if let Some(existing) = lookup_head(&conn, slug, "banner")
            .map_err(|e| format!("lookup_head: {e}"))?
        {
            if existing.source_hash == source_hash {
                return Ok(existing.art_text);
            }
        }
    }

    // 5. prompt — Pillar 37: medium constraint ("rendering target is 72 wide"),
    // not a quota ("produce exactly N lines").
    let system_prompt = "You are an ASCII art generator. The rendering target is a 72-column-wide monospace character grid (like an old terminal or BBS). Use box-drawing characters (┌─┐│└─┘╔═╗║╚═╝), block elements (░▒▓█), and tree connectors (├─└─). Generate art that captures the subject matter thematically. Output ONLY the ASCII art — no explanations, no markdown fences, no commentary.";
    let user_prompt = format!(
        "Generate a thematic ASCII banner for a knowledge pyramid about: \"{}\"\n\nThe banner sits at the top of the pyramid's web page. It should evoke the subject matter visually while staying within 72 columns wide. Aim for something striking, not generic.",
        apex_headline
    );

    // 6. call Grok 4.2 directly
    let config = state.config.read().await.clone();
    let raw = crate::pyramid::llm::call_model_direct(
        &config,
        ASCII_ART_MODEL,
        system_prompt,
        &user_prompt,
        800,
    )
    .await
    .map_err(|e| format!("LLM call failed: {e}"))?;

    // 7. validate (post-hoc)
    let validated = validate_ascii(&raw)?;

    // 8. insert + supersede
    {
        let conn = state.writer.lock().await;
        insert_with_supersession(
            &conn,
            slug,
            "banner",
            &source_hash,
            &validated,
            ASCII_ART_MODEL,
        )
        .map_err(|e| format!("insert_with_supersession: {e}"))?;
    }

    Ok(validated)
}

// ── Validation ──────────────────────────────────────────────────────────────

/// Post-hoc validation per Pillar 37 (width is a medium constraint, not a
/// quota). Strips leading/trailing blank lines, rejects empty, caps each line
/// at 80 chars (a little flex over the 72-col target) and total height at 30
/// lines.
pub fn validate_ascii(text: &str) -> Result<String, String> {
    // Strip markdown fences if the model inserted any, despite being told not
    // to. We trim only the outermost fence lines, NOT leading whitespace on
    // content lines (which is load-bearing for ASCII art indentation).
    let mut lines: Vec<&str> = text.lines().collect();
    if let Some(first) = lines.first() {
        if first.trim_start().starts_with("```") {
            lines.remove(0);
        }
    }
    if let Some(last) = lines.last() {
        if last.trim().starts_with("```") {
            lines.pop();
        }
    }

    // strip leading blank lines
    let mut start = 0;
    while start < lines.len() && lines[start].trim().is_empty() {
        start += 1;
    }
    // strip trailing blank lines
    let mut end = lines.len();
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let stripped = &lines[start..end];

    if stripped.is_empty() {
        return Err("empty art".to_string());
    }

    for line in stripped {
        let width = line.chars().count();
        if width > 80 {
            return Err(format!("line too wide: {} chars", width));
        }
    }

    if stripped.len() > 30 {
        return Err(format!("too tall: {} lines", stripped.len()));
    }

    Ok(stripped.join("\n"))
}

// ── DB row type ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AsciiArtRow {
    pub id: i64,
    pub source_hash: String,
    pub art_text: String,
    pub model: String,
}

/// Return the current (unsuperseded) head row for `(slug, kind)`, if any.
pub fn lookup_head(
    conn: &Connection,
    slug: &str,
    kind: &str,
) -> SqlResult<Option<AsciiArtRow>> {
    conn.query_row(
        "SELECT id, source_hash, art_text, model
           FROM pyramid_ascii_art
          WHERE slug = ?1 AND kind = ?2 AND superseded_by IS NULL
          ORDER BY id DESC
          LIMIT 1",
        params![slug, kind],
        |row| {
            Ok(AsciiArtRow {
                id: row.get(0)?,
                source_hash: row.get(1)?,
                art_text: row.get(2)?,
                model: row.get(3)?,
            })
        },
    )
    .optional()
}

/// INSERT a new row and set the previous head's `superseded_by` to point at
/// it, all in one transaction. We never UPDATE/DELETE existing content — per
/// Pillar 1, 5, the full history chain is preserved.
pub fn insert_with_supersession(
    conn: &Connection,
    slug: &str,
    kind: &str,
    source_hash: &str,
    art_text: &str,
    model: &str,
) -> SqlResult<i64> {
    let tx = conn.unchecked_transaction()?;

    let prev_head_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM pyramid_ascii_art
              WHERE slug = ?1 AND kind = ?2 AND superseded_by IS NULL",
            params![slug, kind],
            |row| row.get(0),
        )
        .optional()?;

    tx.execute(
        "INSERT INTO pyramid_ascii_art (slug, kind, source_hash, art_text, model)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![slug, kind, source_hash, art_text, model],
    )?;
    let new_id = tx.last_insert_rowid();

    if let Some(prev_id) = prev_head_id {
        tx.execute(
            "UPDATE pyramid_ascii_art SET superseded_by = ?1 WHERE id = ?2",
            params![new_id, prev_id],
        )?;
    }

    tx.commit()?;
    Ok(new_id)
}

/// Non-blocking fetch of the current banner (for render.rs / future endpoint).
/// Returns None if the reader lock would block or no banner exists.
pub fn get_banner_for_slug(state: &PyramidState, slug: &str) -> Option<String> {
    let conn = state.reader.try_lock().ok()?;
    lookup_head(&conn, slug, "banner")
        .ok()
        .flatten()
        .map(|r| r.art_text)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE pyramid_ascii_art (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slug TEXT NOT NULL,
                kind TEXT NOT NULL,
                source_hash TEXT NOT NULL,
                art_text TEXT NOT NULL,
                model TEXT NOT NULL,
                superseded_by INTEGER REFERENCES pyramid_ascii_art(id),
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_ascii_art_slug_kind_head
                ON pyramid_ascii_art(slug, kind) WHERE superseded_by IS NULL;",
        )
        .unwrap();
        conn
    }

    #[test]
    fn validate_strips_blank_lines() {
        let input = "\n\n  ┌──┐\n  │  │\n  └──┘\n\n";
        let out = validate_ascii(input).unwrap();
        assert_eq!(out, "  ┌──┐\n  │  │\n  └──┘");
    }

    #[test]
    fn validate_strips_markdown_fences() {
        let input = "```\n┌──┐\n└──┘\n```";
        let out = validate_ascii(input).unwrap();
        assert_eq!(out, "┌──┐\n└──┘");
    }

    #[test]
    fn validate_rejects_empty() {
        assert!(validate_ascii("").is_err());
        assert!(validate_ascii("\n\n   \n").is_err());
    }

    #[test]
    fn validate_rejects_too_wide() {
        let wide = "x".repeat(81);
        assert!(validate_ascii(&wide).is_err());
    }

    #[test]
    fn validate_accepts_up_to_80_cols() {
        let ok = "x".repeat(80);
        assert!(validate_ascii(&ok).is_ok());
    }

    #[test]
    fn validate_rejects_too_tall() {
        let tall: String =
            (0..31).map(|_| "abc".to_string()).collect::<Vec<_>>().join("\n");
        assert!(validate_ascii(&tall).is_err());
    }

    #[test]
    fn insert_with_supersession_chains_old_head() {
        let conn = fresh_db();

        let id1 = insert_with_supersession(&conn, "slug-a", "banner", "h1", "art1", "m").unwrap();
        let id2 = insert_with_supersession(&conn, "slug-a", "banner", "h2", "art2", "m").unwrap();

        // id1 should now point at id2
        let sup: Option<i64> = conn
            .query_row(
                "SELECT superseded_by FROM pyramid_ascii_art WHERE id = ?1",
                params![id1],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sup, Some(id2));

        // id2 is the current head
        let head = lookup_head(&conn, "slug-a", "banner").unwrap().unwrap();
        assert_eq!(head.id, id2);
        assert_eq!(head.art_text, "art2");
    }

    #[test]
    fn lookup_head_only_returns_unsuperseded() {
        let conn = fresh_db();
        insert_with_supersession(&conn, "s", "banner", "h1", "a1", "m").unwrap();
        insert_with_supersession(&conn, "s", "banner", "h2", "a2", "m").unwrap();
        let head = lookup_head(&conn, "s", "banner").unwrap().unwrap();
        assert_eq!(head.art_text, "a2");
    }

    #[test]
    fn supersession_preserves_history() {
        let conn = fresh_db();
        insert_with_supersession(&conn, "s", "banner", "h1", "a1", "m").unwrap();
        insert_with_supersession(&conn, "s", "banner", "h2", "a2", "m").unwrap();
        insert_with_supersession(&conn, "s", "banner", "h3", "a3", "m").unwrap();

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM pyramid_ascii_art", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 3);

        let heads: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_ascii_art WHERE superseded_by IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(heads, 1);
    }

    #[test]
    fn lookup_head_empty_is_none() {
        let conn = fresh_db();
        assert!(lookup_head(&conn, "nope", "banner").unwrap().is_none());
    }

    #[test]
    fn kinds_are_isolated() {
        let conn = fresh_db();
        insert_with_supersession(&conn, "s", "banner", "h1", "b", "m").unwrap();
        insert_with_supersession(&conn, "s", "hero", "h2", "h", "m").unwrap();
        let b = lookup_head(&conn, "s", "banner").unwrap().unwrap();
        let h = lookup_head(&conn, "s", "hero").unwrap().unwrap();
        assert_eq!(b.art_text, "b");
        assert_eq!(h.art_text, "h");
    }
}
