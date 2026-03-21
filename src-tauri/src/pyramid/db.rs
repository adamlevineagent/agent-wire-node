// pyramid/db.rs — SQLite schema, migrations, and CRUD operations for the Knowledge Pyramid
//
// Tables: pyramid_slugs, pyramid_batches, pyramid_nodes, pyramid_chunks, pyramid_pipeline_steps
// All JSON columns (topics, corrections, decisions, terms, dead_ends, children) are stored as
// JSON strings and parsed/serialized via serde_json on read/write.

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::types::*;

// ── Schema Initialization ────────────────────────────────────────────────────

/// Initialize pyramid tables. Call on app startup.
///
/// Enables WAL mode and foreign keys, then creates all five tables with
/// proper constraints and indices.
pub fn init_pyramid_db(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_slugs (
            slug TEXT PRIMARY KEY,
            content_type TEXT NOT NULL CHECK(content_type IN ('code', 'conversation', 'document')),
            source_path TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_built_at TEXT,
            node_count INTEGER NOT NULL DEFAULT 0,
            max_depth INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS pyramid_batches (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            batch_type TEXT NOT NULL DEFAULT 'initial',
            source_path TEXT NOT NULL DEFAULT '',
            chunk_offset INTEGER NOT NULL DEFAULT 0,
            chunk_count INTEGER NOT NULL DEFAULT 0,
            metadata TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS pyramid_nodes (
            id TEXT NOT NULL,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            depth INTEGER NOT NULL,
            chunk_index INTEGER,
            distilled TEXT NOT NULL DEFAULT '',
            topics TEXT,
            corrections TEXT,
            decisions TEXT,
            terms TEXT,
            dead_ends TEXT,
            self_prompt TEXT,
            children TEXT,
            parent_id TEXT,
            build_version INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, id)
        );

        CREATE INDEX IF NOT EXISTS idx_pyramid_nodes_depth ON pyramid_nodes(slug, depth);
        CREATE INDEX IF NOT EXISTS idx_pyramid_nodes_parent ON pyramid_nodes(slug, parent_id);

        CREATE TABLE IF NOT EXISTS pyramid_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            batch_id INTEGER REFERENCES pyramid_batches(id),
            chunk_index INTEGER NOT NULL,
            content TEXT NOT NULL,
            line_count INTEGER,
            char_count INTEGER,
            UNIQUE(slug, chunk_index)
        );

        CREATE TABLE IF NOT EXISTS pyramid_pipeline_steps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            step_type TEXT NOT NULL,
            chunk_index INTEGER NOT NULL DEFAULT -1,
            depth INTEGER NOT NULL DEFAULT -1,
            node_id TEXT NOT NULL DEFAULT '',
            output_json TEXT,
            model TEXT,
            elapsed_seconds REAL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, step_type, chunk_index, depth, node_id)
        );
        ",
    )?;

    Ok(())
}

// ── Slug CRUD ────────────────────────────────────────────────────────────────

/// Create a new slug entry. Returns the created SlugInfo.
pub fn create_slug(
    conn: &Connection,
    slug: &str,
    content_type: &ContentType,
    source_path: &str,
) -> Result<SlugInfo> {
    conn.execute(
        "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, ?2, ?3)",
        rusqlite::params![slug, content_type.as_str(), source_path],
    )
    .with_context(|| format!("Failed to create slug '{slug}'"))?;

    // Read back the row to get server-generated defaults (created_at)
    get_slug(conn, slug)?
        .ok_or_else(|| anyhow::anyhow!("Slug '{slug}' not found after insert"))
}

/// Fetch a slug by name. Returns None if not found.
pub fn get_slug(conn: &Connection, slug: &str) -> Result<Option<SlugInfo>> {
    let mut stmt = conn.prepare(
        "SELECT slug, content_type, source_path, node_count, max_depth, last_built_at, created_at
         FROM pyramid_slugs WHERE slug = ?1",
    )?;

    let result = stmt.query_row(rusqlite::params![slug], |row| {
        let ct_str: String = row.get(1)?;
        Ok(SlugInfo {
            slug: row.get(0)?,
            content_type: ContentType::from_str(&ct_str).unwrap_or(ContentType::Document),
            source_path: row.get(2)?,
            node_count: row.get(3)?,
            max_depth: row.get(4)?,
            last_built_at: row.get(5)?,
            created_at: row.get(6)?,
        })
    });

    match result {
        Ok(info) => Ok(Some(info)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// List all slugs, ordered by created_at descending.
pub fn list_slugs(conn: &Connection) -> Result<Vec<SlugInfo>> {
    let mut stmt = conn.prepare(
        "SELECT slug, content_type, source_path, node_count, max_depth, last_built_at, created_at
         FROM pyramid_slugs ORDER BY created_at DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        let ct_str: String = row.get(1)?;
        Ok(SlugInfo {
            slug: row.get(0)?,
            content_type: ContentType::from_str(&ct_str).unwrap_or(ContentType::Document),
            source_path: row.get(2)?,
            node_count: row.get(3)?,
            max_depth: row.get(4)?,
            last_built_at: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;

    let mut slugs = Vec::new();
    for row in rows {
        slugs.push(row?);
    }
    Ok(slugs)
}

/// Delete a slug and all associated data (cascades to nodes, chunks, batches, steps).
pub fn delete_slug(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

/// Recompute node_count, max_depth, and last_built_at from the nodes table.
pub fn update_slug_stats(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET
            node_count = (SELECT COUNT(*) FROM pyramid_nodes WHERE slug = ?1),
            max_depth = COALESCE((SELECT MAX(depth) FROM pyramid_nodes WHERE slug = ?1), 0),
            last_built_at = datetime('now')
         WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

// ── Batch CRUD ───────────────────────────────────────────────────────────────

/// Create a batch record. Returns the new batch ID.
pub fn create_batch(
    conn: &Connection,
    slug: &str,
    batch_type: &str,
    source_path: &str,
    chunk_offset: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_batches (slug, batch_type, source_path, chunk_offset)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![slug, batch_type, source_path, chunk_offset],
    )?;
    Ok(conn.last_insert_rowid())
}

// ── Chunk CRUD ───────────────────────────────────────────────────────────────

/// Insert a chunk. Computes line_count and char_count automatically.
pub fn insert_chunk(
    conn: &Connection,
    slug: &str,
    batch_id: i64,
    chunk_index: i64,
    content: &str,
) -> Result<()> {
    let line_count = content.matches('\n').count() as i64 + 1;
    let char_count = content.len() as i64;

    conn.execute(
        "INSERT INTO pyramid_chunks (slug, batch_id, chunk_index, content, line_count, char_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![slug, batch_id, chunk_index, content, line_count, char_count],
    )?;

    // Update batch chunk_count
    conn.execute(
        "UPDATE pyramid_batches SET chunk_count = chunk_count + 1 WHERE id = ?1",
        rusqlite::params![batch_id],
    )?;

    Ok(())
}

/// Get chunk content by slug and chunk_index.
pub fn get_chunk(conn: &Connection, slug: &str, chunk_index: i64) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT content FROM pyramid_chunks WHERE slug = ?1 AND chunk_index = ?2",
    )?;

    let result = stmt.query_row(rusqlite::params![slug, chunk_index], |row| {
        row.get::<_, String>(0)
    });

    match result {
        Ok(content) => Ok(Some(content)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Count total chunks for a slug.
pub fn count_chunks(conn: &Connection, slug: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_chunks WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;
    Ok(count)
}

// ── Node CRUD ────────────────────────────────────────────────────────────────

/// Helper: parse a JSON column string into a Vec<T>, returning empty vec on null/error.
fn parse_json_col<T: serde::de::DeserializeOwned>(json_str: Option<String>) -> Vec<T> {
    match json_str {
        Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Helper: parse a JSON column string, returning empty string on null/error.
fn parse_json_string(json_str: Option<String>) -> String {
    json_str.unwrap_or_default()
}

/// Build a PyramidNode from a rusqlite Row.
///
/// Expected column order:
///   0:id, 1:slug, 2:depth, 3:chunk_index, 4:distilled, 5:topics,
///   6:corrections, 7:decisions, 8:terms, 9:dead_ends, 10:self_prompt,
///   11:children, 12:parent_id, 13:created_at
fn node_from_row(row: &rusqlite::Row) -> rusqlite::Result<PyramidNode> {
    let topics_json: Option<String> = row.get(5)?;
    let corrections_json: Option<String> = row.get(6)?;
    let decisions_json: Option<String> = row.get(7)?;
    let terms_json: Option<String> = row.get(8)?;
    let dead_ends_json: Option<String> = row.get(9)?;
    let self_prompt_raw: Option<String> = row.get(10)?;
    let children_json: Option<String> = row.get(11)?;

    Ok(PyramidNode {
        id: row.get(0)?,
        slug: row.get(1)?,
        depth: row.get(2)?,
        chunk_index: row.get(3)?,
        distilled: row.get(4)?,
        topics: parse_json_col(topics_json),
        corrections: parse_json_col(corrections_json),
        decisions: parse_json_col(decisions_json),
        terms: parse_json_col(terms_json),
        dead_ends: parse_json_col(dead_ends_json),
        self_prompt: parse_json_string(self_prompt_raw),
        children: parse_json_col(children_json),
        parent_id: row.get(12)?,
        created_at: row.get(13)?,
    })
}

const NODE_SELECT_COLS: &str =
    "id, slug, depth, chunk_index, distilled, topics, corrections, decisions, \
     terms, dead_ends, self_prompt, children, parent_id, created_at";

/// Save (upsert) a PyramidNode. Serializes all Vec fields to JSON strings.
///
/// The optional `topics_json` parameter allows passing a pre-serialized topics
/// string (useful when the build pipeline already has the raw JSON). If None,
/// topics are serialized from `node.topics`.
pub fn save_node(
    conn: &Connection,
    node: &PyramidNode,
    topics_json: Option<&str>,
) -> Result<()> {
    let topics = match topics_json {
        Some(s) => s.to_string(),
        None => serde_json::to_string(&node.topics)?,
    };
    let corrections = serde_json::to_string(&node.corrections)?;
    let decisions = serde_json::to_string(&node.decisions)?;
    let terms = serde_json::to_string(&node.terms)?;
    let dead_ends = serde_json::to_string(&node.dead_ends)?;
    let children = serde_json::to_string(&node.children)?;

    conn.execute(
        "INSERT INTO pyramid_nodes
            (id, slug, depth, chunk_index, distilled, topics, corrections, decisions,
             terms, dead_ends, self_prompt, children, parent_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(slug, id) DO UPDATE SET
            depth = excluded.depth,
            chunk_index = excluded.chunk_index,
            distilled = excluded.distilled,
            topics = excluded.topics,
            corrections = excluded.corrections,
            decisions = excluded.decisions,
            terms = excluded.terms,
            dead_ends = excluded.dead_ends,
            self_prompt = excluded.self_prompt,
            children = excluded.children,
            parent_id = excluded.parent_id,
            build_version = build_version + 1",
        rusqlite::params![
            node.id,
            node.slug,
            node.depth,
            node.chunk_index,
            node.distilled,
            topics,
            corrections,
            decisions,
            terms,
            dead_ends,
            node.self_prompt,
            children,
            node.parent_id,
        ],
    )?;

    Ok(())
}

/// Get a single node by slug and node ID.
pub fn get_node(conn: &Connection, slug: &str, node_id: &str) -> Result<Option<PyramidNode>> {
    let sql = format!(
        "SELECT {NODE_SELECT_COLS} FROM pyramid_nodes WHERE slug = ?1 AND id = ?2"
    );
    let mut stmt = conn.prepare(&sql)?;

    let result = stmt.query_row(rusqlite::params![slug, node_id], node_from_row);

    match result {
        Ok(node) => Ok(Some(node)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get all nodes at a given depth for a slug, ordered by chunk_index.
pub fn get_nodes_at_depth(
    conn: &Connection,
    slug: &str,
    depth: i64,
) -> Result<Vec<PyramidNode>> {
    let sql = format!(
        "SELECT {NODE_SELECT_COLS} FROM pyramid_nodes
         WHERE slug = ?1 AND depth = ?2
         ORDER BY chunk_index ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;

    let rows = stmt.query_map(rusqlite::params![slug, depth], node_from_row)?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row?);
    }
    Ok(nodes)
}

/// Count nodes at a given depth for a slug.
pub fn count_nodes_at_depth(conn: &Connection, slug: &str, depth: i64) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_nodes WHERE slug = ?1 AND depth = ?2",
        rusqlite::params![slug, depth],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Delete all nodes with depth > the given depth. Returns count of deleted rows.
/// Used when rebuilding upper layers of the pyramid.
pub fn delete_nodes_above(conn: &Connection, slug: &str, depth: i64) -> Result<i64> {
    let deleted = conn.execute(
        "DELETE FROM pyramid_nodes WHERE slug = ?1 AND depth > ?2",
        rusqlite::params![slug, depth],
    )?;
    Ok(deleted as i64)
}

// ── Pipeline Step Tracking ───────────────────────────────────────────────────

/// Save a pipeline step record (for resumability).
pub fn save_step(
    conn: &Connection,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
    output_json: &str,
    model: &str,
    elapsed: f64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_pipeline_steps
            (slug, step_type, chunk_index, depth, node_id, output_json, model, elapsed_seconds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(slug, step_type, chunk_index, depth, node_id) DO UPDATE SET
            output_json = excluded.output_json,
            model = excluded.model,
            elapsed_seconds = excluded.elapsed_seconds",
        rusqlite::params![slug, step_type, chunk_index, depth, node_id, output_json, model, elapsed],
    )?;
    Ok(())
}

/// Check whether a specific pipeline step has already been completed.
pub fn step_exists(
    conn: &Connection,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_pipeline_steps
         WHERE slug = ?1 AND step_type = ?2 AND chunk_index = ?3 AND depth = ?4 AND node_id = ?5",
        rusqlite::params![slug, step_type, chunk_index, depth, node_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Get the output_json for a step, looked up by slug + step_type + chunk_index.
/// Returns the most recent match (by id DESC) if multiple exist at different depths.
pub fn get_step_output(
    conn: &Connection,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT output_json FROM pyramid_pipeline_steps
         WHERE slug = ?1 AND step_type = ?2 AND chunk_index = ?3
         ORDER BY id DESC LIMIT 1",
    )?;

    let result = stmt.query_row(rusqlite::params![slug, step_type, chunk_index], |row| {
        row.get::<_, Option<String>>(0)
    });

    match result {
        Ok(json) => Ok(json),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Delete all pipeline steps of a given type for a slug.
/// Used when re-running a specific pipeline phase.
pub fn delete_steps(conn: &Connection, slug: &str, step_type: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_pipeline_steps WHERE slug = ?1 AND step_type = ?2",
        rusqlite::params![slug, step_type],
    )?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_init_creates_tables() {
        let conn = test_conn();
        // Verify tables exist by querying them
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pyramid_slugs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_slug_crud() {
        let conn = test_conn();

        // Create
        let info = create_slug(&conn, "test-slug", &ContentType::Code, "/src").unwrap();
        assert_eq!(info.slug, "test-slug");
        assert_eq!(info.node_count, 0);

        // Get
        let got = get_slug(&conn, "test-slug").unwrap().unwrap();
        assert_eq!(got.source_path, "/src");

        // List
        let all = list_slugs(&conn).unwrap();
        assert_eq!(all.len(), 1);

        // Delete
        delete_slug(&conn, "test-slug").unwrap();
        assert!(get_slug(&conn, "test-slug").unwrap().is_none());
    }

    #[test]
    fn test_chunk_crud() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Conversation, "").unwrap();
        let batch_id = create_batch(&conn, "s", "initial", "/path", 0).unwrap();

        insert_chunk(&conn, "s", batch_id, 0, "hello world\nsecond line").unwrap();
        insert_chunk(&conn, "s", batch_id, 1, "chunk two").unwrap();

        assert_eq!(count_chunks(&conn, "s").unwrap(), 2);

        let content = get_chunk(&conn, "s", 0).unwrap().unwrap();
        assert!(content.contains("hello world"));

        assert!(get_chunk(&conn, "s", 99).unwrap().is_none());
    }

    #[test]
    fn test_node_save_and_get() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let node = PyramidNode {
            id: "n1".to_string(),
            slug: "s".to_string(),
            depth: 0,
            chunk_index: Some(0),
            distilled: "Test distillation".to_string(),
            topics: vec![Topic {
                name: "Auth".to_string(),
                current: "JWT-based auth".to_string(),
                entities: vec!["AuthState".to_string()],
                corrections: vec![],
                decisions: vec![Decision {
                    decided: "Use JWT".to_string(),
                    why: "Standard".to_string(),
                }],
            }],
            corrections: vec![],
            decisions: vec![],
            terms: vec![Term {
                term: "JWT".to_string(),
                definition: "JSON Web Token".to_string(),
            }],
            dead_ends: vec!["OAuth considered".to_string()],
            self_prompt: "What auth mechanism?".to_string(),
            children: vec!["c1".to_string(), "c2".to_string()],
            parent_id: None,
            created_at: String::new(),
        };

        save_node(&conn, &node, None).unwrap();

        let got = get_node(&conn, "s", "n1").unwrap().unwrap();
        assert_eq!(got.distilled, "Test distillation");
        assert_eq!(got.topics.len(), 1);
        assert_eq!(got.topics[0].name, "Auth");
        assert_eq!(got.topics[0].decisions.len(), 1);
        assert_eq!(got.terms.len(), 1);
        assert_eq!(got.dead_ends.len(), 1);
        assert_eq!(got.children.len(), 2);
    }

    #[test]
    fn test_nodes_at_depth() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        for i in 0..3 {
            let node = PyramidNode {
                id: format!("d0-{i}"),
                slug: "s".to_string(),
                depth: 0,
                chunk_index: Some(i),
                distilled: format!("Node {i}"),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                created_at: String::new(),
            };
            save_node(&conn, &node, None).unwrap();
        }

        let depth0 = get_nodes_at_depth(&conn, "s", 0).unwrap();
        assert_eq!(depth0.len(), 3);

        let depth1 = get_nodes_at_depth(&conn, "s", 1).unwrap();
        assert_eq!(depth1.len(), 0);

        assert_eq!(count_nodes_at_depth(&conn, "s", 0).unwrap(), 3);
    }

    #[test]
    fn test_delete_nodes_above() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        for depth in 0..4 {
            let node = PyramidNode {
                id: format!("d{depth}"),
                slug: "s".to_string(),
                depth,
                chunk_index: None,
                distilled: format!("Depth {depth}"),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                created_at: String::new(),
            };
            save_node(&conn, &node, None).unwrap();
        }

        let deleted = delete_nodes_above(&conn, "s", 1).unwrap();
        assert_eq!(deleted, 2); // depth 2 and 3

        // depth 0 and 1 remain
        assert_eq!(count_nodes_at_depth(&conn, "s", 0).unwrap(), 1);
        assert_eq!(count_nodes_at_depth(&conn, "s", 1).unwrap(), 1);
        assert_eq!(count_nodes_at_depth(&conn, "s", 2).unwrap(), 0);
    }

    #[test]
    fn test_pipeline_steps() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        assert!(!step_exists(&conn, "s", "extract", 0, 0, "").unwrap());

        save_step(&conn, "s", "extract", 0, 0, "", r#"{"ok":true}"#, "gpt-4", 1.5).unwrap();
        assert!(step_exists(&conn, "s", "extract", 0, 0, "").unwrap());

        let output = get_step_output(&conn, "s", "extract", 0).unwrap().unwrap();
        assert!(output.contains("ok"));

        // Upsert overwrites
        save_step(&conn, "s", "extract", 0, 0, "", r#"{"ok":false}"#, "gpt-4", 2.0).unwrap();
        let output2 = get_step_output(&conn, "s", "extract", 0).unwrap().unwrap();
        assert!(output2.contains("false"));

        // Delete steps
        delete_steps(&conn, "s", "extract").unwrap();
        assert!(!step_exists(&conn, "s", "extract", 0, 0, "").unwrap());
    }

    #[test]
    fn test_update_slug_stats() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        // Add nodes at two depths
        for i in 0..3 {
            let node = PyramidNode {
                id: format!("d0-{i}"),
                slug: "s".to_string(),
                depth: 0,
                chunk_index: Some(i),
                distilled: String::new(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                created_at: String::new(),
            };
            save_node(&conn, &node, None).unwrap();
        }
        let apex = PyramidNode {
            id: "apex".to_string(),
            slug: "s".to_string(),
            depth: 1,
            chunk_index: None,
            distilled: String::new(),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec!["d0-0".into(), "d0-1".into(), "d0-2".into()],
            parent_id: None,
            created_at: String::new(),
        };
        save_node(&conn, &apex, None).unwrap();

        update_slug_stats(&conn, "s").unwrap();

        let info = get_slug(&conn, "s").unwrap().unwrap();
        assert_eq!(info.node_count, 4);
        assert_eq!(info.max_depth, 1);
        assert!(info.last_built_at.is_some());
    }

    #[test]
    fn test_node_upsert() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let mut node = PyramidNode {
            id: "n1".to_string(),
            slug: "s".to_string(),
            depth: 0,
            chunk_index: Some(0),
            distilled: "Version 1".to_string(),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec![],
            parent_id: None,
            created_at: String::new(),
        };
        save_node(&conn, &node, None).unwrap();

        // Upsert with new content
        node.distilled = "Version 2".to_string();
        save_node(&conn, &node, None).unwrap();

        let got = get_node(&conn, "s", "n1").unwrap().unwrap();
        assert_eq!(got.distilled, "Version 2");

        // Should still be 1 node, not 2
        assert_eq!(count_nodes_at_depth(&conn, "s", 0).unwrap(), 1);
    }
}
