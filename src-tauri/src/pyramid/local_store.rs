//! Local storage for build metadata — tracks build progress, completion, and quality.

use anyhow::{anyhow, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

/// Metadata for a single pyramid build.
#[derive(Debug, Clone, Serialize)]
pub struct BuildMetadata {
    pub slug: String,
    pub build_id: String,
    pub question: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub status: String,
    pub layers_completed: i64,
    pub total_layers: i64,
    pub l0_node_count: i64,
    pub total_node_count: i64,
    pub quality_score: Option<f64>,
    pub error_message: Option<String>,
}

/// Record the start of a new build.
/// 11-X: `question` = enhanced question, `original_question` = user's original.
/// original_question is preserved on conflict — never overwritten on re-run.
pub fn save_build_start(
    conn: &Connection,
    slug: &str,
    build_id: &str,
    question: &str,
    total_layers: i64,
    original_question: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_builds (slug, build_id, question, total_layers, status, original_question)
         VALUES (?1, ?2, ?3, ?4, 'running', ?5)
         ON CONFLICT(slug, build_id) DO UPDATE SET
           question = excluded.question,
           total_layers = excluded.total_layers,
           status = 'running',
           started_at = datetime('now'),
           completed_at = NULL,
           error_message = NULL,
           original_question = COALESCE(pyramid_builds.original_question, excluded.original_question)",
        rusqlite::params![slug, build_id, question, total_layers, original_question],
    )?;
    Ok(())
}

/// Update build progress (call after each layer completes).
pub fn update_build_progress(
    conn: &Connection,
    slug: &str,
    build_id: &str,
    layers_completed: i64,
    l0_node_count: i64,
    total_node_count: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_builds SET
           layers_completed = ?1, l0_node_count = ?2, total_node_count = ?3
         WHERE slug = ?4 AND build_id = ?5",
        rusqlite::params![
            layers_completed,
            l0_node_count,
            total_node_count,
            slug,
            build_id
        ],
    )?;
    Ok(())
}

/// Mark a build as successfully completed.
pub fn complete_build(
    conn: &Connection,
    slug: &str,
    build_id: &str,
    quality_score: Option<f64>,
) -> Result<()> {
    complete_build_inner(conn, slug, build_id, quality_score, false)
}

/// Mark a build as successfully completed even when it intentionally
/// processed an empty corpus. Regular callers should use `complete_build`.
#[allow(dead_code)]
pub fn complete_empty_build(
    conn: &Connection,
    slug: &str,
    build_id: &str,
    quality_score: Option<f64>,
) -> Result<()> {
    complete_build_inner(conn, slug, build_id, quality_score, true)
}

fn complete_build_inner(
    conn: &Connection,
    slug: &str,
    build_id: &str,
    quality_score: Option<f64>,
    allow_empty: bool,
) -> Result<()> {
    let total_node_count = conn
        .query_row(
            "SELECT total_node_count FROM pyramid_builds WHERE slug = ?1 AND build_id = ?2",
            rusqlite::params![slug, build_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("build {build_id} for slug {slug} does not exist"))?;

    if !allow_empty && total_node_count <= 0 {
        return Err(anyhow!(
            "refusing to complete build {build_id} for slug {slug}: total_node_count is {total_node_count}; use complete_empty_build for an explicit empty corpus"
        ));
    }

    conn.execute(
        "UPDATE pyramid_builds SET
           status = 'completed',
           completed_at = datetime('now'),
           quality_score = ?1
         WHERE slug = ?2 AND build_id = ?3",
        rusqlite::params![quality_score, slug, build_id],
    )?;
    Ok(())
}

/// Mark a build as failed.
pub fn fail_build(
    conn: &Connection,
    slug: &str,
    build_id: &str,
    error_message: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_builds SET
           status = 'failed',
           completed_at = datetime('now'),
           error_message = ?1
         WHERE slug = ?2 AND build_id = ?3",
        rusqlite::params![error_message, slug, build_id],
    )?;
    Ok(())
}

/// Get the latest build for a slug (by started_at descending).
pub fn get_latest_build(conn: &Connection, slug: &str) -> Result<Option<BuildMetadata>> {
    let mut stmt = conn.prepare(
        "SELECT slug, build_id, question, started_at, completed_at, status,
                layers_completed, total_layers, l0_node_count, total_node_count,
                quality_score, error_message
         FROM pyramid_builds
         WHERE slug = ?1
         ORDER BY started_at DESC
         LIMIT 1",
    )?;

    let result = stmt.query_row(rusqlite::params![slug], |row| {
        Ok(BuildMetadata {
            slug: row.get(0)?,
            build_id: row.get(1)?,
            question: row.get(2)?,
            started_at: row.get(3)?,
            completed_at: row.get(4)?,
            status: row.get(5)?,
            layers_completed: row.get(6)?,
            total_layers: row.get(7)?,
            l0_node_count: row.get(8)?,
            total_node_count: row.get(9)?,
            quality_score: row.get(10)?,
            error_message: row.get(11)?,
        })
    });

    match result {
        Ok(meta) => Ok(Some(meta)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE pyramid_builds (
                slug TEXT NOT NULL,
                build_id TEXT NOT NULL,
                question TEXT NOT NULL,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                completed_at TEXT,
                status TEXT NOT NULL DEFAULT 'running',
                layers_completed INTEGER DEFAULT 0,
                total_layers INTEGER DEFAULT 0,
                l0_node_count INTEGER DEFAULT 0,
                total_node_count INTEGER DEFAULT 0,
                quality_score REAL,
                error_message TEXT,
                original_question TEXT DEFAULT NULL,
                PRIMARY KEY (slug, build_id)
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_build_lifecycle() {
        let conn = setup_db();
        save_build_start(&conn, "test", "b1", "What is this?", 3, None).unwrap();

        let meta = get_latest_build(&conn, "test").unwrap().unwrap();
        assert_eq!(meta.status, "running");
        assert_eq!(meta.total_layers, 3);

        update_build_progress(&conn, "test", "b1", 1, 30, 35).unwrap();
        let meta = get_latest_build(&conn, "test").unwrap().unwrap();
        assert_eq!(meta.layers_completed, 1);
        assert_eq!(meta.l0_node_count, 30);

        complete_build(&conn, "test", "b1", Some(7.5)).unwrap();
        let meta = get_latest_build(&conn, "test").unwrap().unwrap();
        assert_eq!(meta.status, "completed");
        assert_eq!(meta.quality_score, Some(7.5));
    }

    #[test]
    fn test_build_failure() {
        let conn = setup_db();
        save_build_start(&conn, "test", "b2", "Why?", 2, None).unwrap();
        fail_build(&conn, "test", "b2", "LLM timeout").unwrap();

        let meta = get_latest_build(&conn, "test").unwrap().unwrap();
        assert_eq!(meta.status, "failed");
        assert_eq!(meta.error_message.as_deref(), Some("LLM timeout"));
    }

    #[test]
    fn test_complete_build_rejects_zero_node_false_success() {
        let conn = setup_db();
        save_build_start(&conn, "test", "b3", "Empty?", 1, None).unwrap();

        let err = complete_build(&conn, "test", "b3", None).unwrap_err();
        assert!(
            err.to_string().contains("total_node_count is 0"),
            "expected zero-node guard, got {err}"
        );

        let meta = get_latest_build(&conn, "test").unwrap().unwrap();
        assert_eq!(meta.status, "running");
        assert!(meta.completed_at.is_none());
    }

    #[test]
    fn test_complete_empty_build_is_explicit_escape_hatch() {
        let conn = setup_db();
        save_build_start(&conn, "test", "b4", "Empty on purpose?", 1, None).unwrap();

        complete_empty_build(&conn, "test", "b4", None).unwrap();

        let meta = get_latest_build(&conn, "test").unwrap().unwrap();
        assert_eq!(meta.status, "completed");
        assert_eq!(meta.total_node_count, 0);
    }

    #[test]
    fn test_no_build_returns_none() {
        let conn = setup_db();
        assert!(get_latest_build(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_original_question_preserved_on_rebuild() {
        let conn = setup_db();
        // First insert: both question and original_question set
        save_build_start(&conn, "test", "b1", "Enhanced?", 3, Some("Original?")).unwrap();

        let row: String = conn
            .query_row(
                "SELECT original_question FROM pyramid_builds WHERE slug = 'test' AND build_id = 'b1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row, "Original?");

        // Re-run with different original_question — COALESCE should preserve the first one
        save_build_start(
            &conn,
            "test",
            "b1",
            "Enhanced v2?",
            4,
            Some("Different original?"),
        )
        .unwrap();

        let row2: String = conn
            .query_row(
                "SELECT original_question FROM pyramid_builds WHERE slug = 'test' AND build_id = 'b1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            row2, "Original?",
            "COALESCE should preserve original_question on re-run"
        );

        // question column should be updated to the new enhanced version
        let meta = get_latest_build(&conn, "test").unwrap().unwrap();
        assert_eq!(meta.question, "Enhanced v2?");
    }
}
