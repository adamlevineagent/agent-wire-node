//! Local storage for build metadata — tracks build progress, completion, and quality.

use anyhow::Result;
use rusqlite::Connection;
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
pub fn save_build_start(
    conn: &Connection,
    slug: &str,
    build_id: &str,
    question: &str,
    total_layers: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_builds (slug, build_id, question, total_layers, status)
         VALUES (?1, ?2, ?3, ?4, 'running')
         ON CONFLICT(slug, build_id) DO UPDATE SET
           question = excluded.question,
           total_layers = excluded.total_layers,
           status = 'running',
           started_at = datetime('now'),
           completed_at = NULL,
           error_message = NULL",
        rusqlite::params![slug, build_id, question, total_layers],
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
        rusqlite::params![layers_completed, l0_node_count, total_node_count, slug, build_id],
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
                PRIMARY KEY (slug, build_id)
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_build_lifecycle() {
        let conn = setup_db();
        save_build_start(&conn, "test", "b1", "What is this?", 3).unwrap();

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
        save_build_start(&conn, "test", "b2", "Why?", 2).unwrap();
        fail_build(&conn, "test", "b2", "LLM timeout").unwrap();

        let meta = get_latest_build(&conn, "test").unwrap().unwrap();
        assert_eq!(meta.status, "failed");
        assert_eq!(meta.error_message.as_deref(), Some("LLM timeout"));
    }

    #[test]
    fn test_no_build_returns_none() {
        let conn = setup_db();
        assert!(get_latest_build(&conn, "nonexistent").unwrap().is_none());
    }
}
