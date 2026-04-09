use anyhow::Result;
use rusqlite::Connection;

/// Initialize the chain assignment table. Call during init_pyramid_db().
pub fn init_chain_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_chain_assignments (
            slug TEXT PRIMARY KEY REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            chain_id TEXT NOT NULL,
            chain_file TEXT,
            assigned_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        ",
    )?;
    Ok(())
}

/// Assign a chain to a pyramid slug.
pub fn assign_chain(
    conn: &Connection,
    slug: &str,
    chain_id: &str,
    chain_file: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_chain_assignments (slug, chain_id, chain_file)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(slug) DO UPDATE SET chain_id = excluded.chain_id,
                                         chain_file = excluded.chain_file,
                                         assigned_at = datetime('now')",
        rusqlite::params![slug, chain_id, chain_file],
    )?;
    Ok(())
}

/// Get the chain assignment for a slug. Returns (chain_id, chain_file) or None.
pub fn get_assignment(conn: &Connection, slug: &str) -> Result<Option<(String, Option<String>)>> {
    let mut stmt =
        conn.prepare("SELECT chain_id, chain_file FROM pyramid_chain_assignments WHERE slug = ?1")?;
    let result = stmt.query_row(rusqlite::params![slug], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    });
    match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Remove chain assignment for a slug (falls back to default).
pub fn remove_assignment(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_chain_assignments WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

/// List all assignments. Returns Vec of (slug, chain_id, chain_file).
pub fn list_assignments(conn: &Connection) -> Result<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT slug, chain_id, chain_file FROM pyramid_chain_assignments ORDER BY slug",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get the default chain ID for a content type.
///
/// **Conversation slugs default to `conversation-episodic`** — the episodic
/// memory chain (forward + reverse + combine triple-pass L0 producing the
/// unified episodic schema, chronological phase decomposition, evidence
/// grounding, and recursive pair-adjacent synthesis). This is the agent's
/// externalized persistent brain.
///
/// The prior default `conversation-chronological` (retro / meta-learning)
/// remains available as an alternative chain. Operators can switch to it
/// via explicit slug-specific assignment in `pyramid_chain_assignments`.
///
/// All other content types route to `question-pipeline` — the question.yaml
/// chain with decompose → answer flow. Legacy chains (`code-default`,
/// `document-default`, `conversation-default`) are DEPRECATED.
///
/// Operators with an explicit slug-specific assignment in
/// `pyramid_chain_assignments` still get their assigned chain — this only
/// affects builds that rely on the default.
pub fn default_chain_id(content_type: &str) -> &'static str {
    default_chain_id_for_mode(content_type, "deep")
}

/// Get the default chain ID for a content type and evidence mode.
/// "fast" mode uses a stripped-down chain that skips evidence grounding.
pub fn default_chain_id_for_mode(content_type: &str, evidence_mode: &str) -> &'static str {
    match (content_type, evidence_mode) {
        ("conversation", "fast") => "conversation-episodic-fast",
        ("conversation", _) => "conversation-episodic",
        _ => "question-pipeline",
    }
}
