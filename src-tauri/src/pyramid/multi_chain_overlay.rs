// pyramid/multi_chain_overlay.rs — WS-MULTI-CHAIN-OVERLAY
//
// Multi-chain overlay: same source content can have multiple pyramids built
// via different chain configurations. The chunker cost is paid once; the
// synthesis cost scales with the number of chains.
//
// This module provides:
//   - Schema initialization (pyramid_chain_overlays table)
//   - CRUD helpers for overlay relationships
//   - create_overlay_build: validates ingest_signature match, registers overlay
//   - Tests for the four required scenarios

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use super::db;
use super::ingest;
use super::types::ChainOverlay;

// ── Schema ──────────────────────────────────────────────────────────────────

/// Initialize the chain overlay tracking table. Called from init_pyramid_db().
pub fn init_overlay_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_chain_overlays (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_slug TEXT NOT NULL,
            overlay_slug TEXT NOT NULL,
            chain_id TEXT NOT NULL,
            ingest_signature TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'active',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(source_slug, overlay_slug)
        );
        CREATE INDEX IF NOT EXISTS idx_chain_overlays_source
            ON pyramid_chain_overlays(source_slug);
        CREATE INDEX IF NOT EXISTS idx_chain_overlays_overlay
            ON pyramid_chain_overlays(overlay_slug);
        ",
    )?;
    Ok(())
}

// ── Row parsing ─────────────────────────────────────────────────────────────

fn parse_overlay(row: &rusqlite::Row) -> rusqlite::Result<ChainOverlay> {
    Ok(ChainOverlay {
        id: row.get(0)?,
        source_slug: row.get(1)?,
        overlay_slug: row.get(2)?,
        chain_id: row.get(3)?,
        ingest_signature: row.get(4)?,
        status: row.get(5)?,
        created_at: row.get(6)?,
    })
}

// ── CRUD helpers ────────────────────────────────────────────────────────────

/// Register a new chain overlay relationship between source_slug and overlay_slug.
pub fn add_chain_overlay(
    conn: &Connection,
    source_slug: &str,
    overlay_slug: &str,
    chain_id: &str,
    ingest_signature: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_chain_overlays (source_slug, overlay_slug, chain_id, ingest_signature)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![source_slug, overlay_slug, chain_id, ingest_signature],
    )
    .context("Failed to insert chain overlay")?;
    Ok(())
}

/// Get all overlays for a given source slug (active only).
pub fn get_overlays_for_source(conn: &Connection, source_slug: &str) -> Result<Vec<ChainOverlay>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_slug, overlay_slug, chain_id, ingest_signature, status, created_at
         FROM pyramid_chain_overlays
         WHERE source_slug = ?1 AND status = 'active'
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![source_slug], parse_overlay)?;
    let mut overlays = Vec::new();
    for row in rows {
        overlays.push(row?);
    }
    Ok(overlays)
}

/// Get the source slug for a given overlay slug. Returns None if the overlay
/// slug is not registered as an overlay of any source.
pub fn get_source_for_overlay(conn: &Connection, overlay_slug: &str) -> Result<Option<String>> {
    let result: Option<String> = conn
        .query_row(
            "SELECT source_slug FROM pyramid_chain_overlays
             WHERE overlay_slug = ?1 AND status = 'active'",
            rusqlite::params![overlay_slug],
            |row| row.get(0),
        )
        .optional()?;
    Ok(result)
}

/// Soft-delete an overlay relationship (sets status to 'removed').
pub fn remove_chain_overlay(
    conn: &Connection,
    source_slug: &str,
    overlay_slug: &str,
) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE pyramid_chain_overlays SET status = 'removed'
         WHERE source_slug = ?1 AND overlay_slug = ?2 AND status = 'active'",
        rusqlite::params![source_slug, overlay_slug],
    )?;
    Ok(rows > 0)
}

/// Check whether a candidate ingest_signature matches the source slug's
/// existing ingest records. Returns true if at least one ingest record for
/// the source slug has a matching signature.
pub fn check_ingest_signature_match(
    conn: &Connection,
    source_slug: &str,
    candidate_signature: &str,
) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_ingest_records
             WHERE slug = ?1 AND ingest_signature = ?2",
            rusqlite::params![source_slug, candidate_signature],
            |row| row.get(0),
        )
        .unwrap_or(0);
    count > 0
}

// ── Overlay build creation ──────────────────────────────────────────────────

/// Create an overlay build: validates that the source slug exists, computes
/// the ingest_signature for the new chain's content type, verifies that
/// signatures match (shared chunking), and registers the overlay relationship.
///
/// The actual build is triggered separately via DADBEAR or manually.
pub fn create_overlay_build(
    conn: &Connection,
    source_slug: &str,
    new_slug: &str,
    chain_id: &str,
) -> Result<()> {
    // 1. Check source slug exists
    let source_info = db::get_slug(conn, source_slug)?
        .ok_or_else(|| anyhow::anyhow!("Source slug '{}' does not exist", source_slug))?;

    // 2. Compute ingest_signature for the new chain using the source's content type
    let config = ingest::default_ingest_config();
    let sig = ingest::ingest_signature(&source_info.content_type, &config);

    // 3. Verify signatures match (shared chunking).
    //    For Vine/Question types the signature is "slug-unique" — overlays
    //    don't make sense for those because there's no shared chunking.
    if sig == "slug-unique" {
        anyhow::bail!(
            "Cannot create overlay for content type '{}' — no shared chunking",
            source_info.content_type.as_str()
        );
    }
    if !check_ingest_signature_match(conn, source_slug, &sig) {
        anyhow::bail!(
            "Ingest signature mismatch: source '{}' has no ingest records with signature '{}'",
            source_slug,
            sig
        );
    }

    // 4. Register the overlay relationship
    add_chain_overlay(conn, source_slug, new_slug, chain_id, &sig)?;

    tracing::info!(
        source_slug = %source_slug,
        overlay_slug = %new_slug,
        chain_id = %chain_id,
        ingest_signature = %sig,
        "Registered chain overlay"
    );

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::types::ContentType;

    /// Set up an in-memory DB with schema + test data for overlay tests.
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        // Create a source slug
        db::create_slug(&conn, "source-pyramid", &ContentType::Code, "/src").unwrap();

        // Insert an ingest record so signature matching works
        let config = ingest::default_ingest_config();
        let sig = ingest::ingest_signature(&ContentType::Code, &config);
        let record = crate::pyramid::types::IngestRecord {
            id: 0,
            slug: "source-pyramid".to_string(),
            source_path: "/src/main.rs".to_string(),
            content_type: "code".to_string(),
            ingest_signature: sig,
            file_hash: Some("abc123".to_string()),
            file_mtime: Some("2026-04-08T10:00:00Z".to_string()),
            status: "complete".to_string(),
            build_id: Some("build-001".to_string()),
            error_message: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        db::save_ingest_record(&conn, &record).unwrap();

        conn
    }

    #[test]
    fn test_add_overlay_and_retrieve() {
        let conn = setup_test_db();
        let config = ingest::default_ingest_config();
        let sig = ingest::ingest_signature(&ContentType::Code, &config);

        // Add overlay
        add_chain_overlay(&conn, "source-pyramid", "retro-overlay", "chain-retro", &sig).unwrap();

        // Retrieve overlays for source
        let overlays = get_overlays_for_source(&conn, "source-pyramid").unwrap();
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].source_slug, "source-pyramid");
        assert_eq!(overlays[0].overlay_slug, "retro-overlay");
        assert_eq!(overlays[0].chain_id, "chain-retro");
        assert_eq!(overlays[0].ingest_signature, sig);
        assert_eq!(overlays[0].status, "active");
    }

    #[test]
    fn test_get_overlays_returns_all_for_source() {
        let conn = setup_test_db();
        let config = ingest::default_ingest_config();
        let sig = ingest::ingest_signature(&ContentType::Code, &config);

        // Add multiple overlays
        add_chain_overlay(&conn, "source-pyramid", "overlay-a", "chain-a", &sig).unwrap();
        add_chain_overlay(&conn, "source-pyramid", "overlay-b", "chain-b", &sig).unwrap();
        add_chain_overlay(&conn, "source-pyramid", "overlay-c", "chain-c", &sig).unwrap();

        let overlays = get_overlays_for_source(&conn, "source-pyramid").unwrap();
        assert_eq!(overlays.len(), 3);

        // Also test get_source_for_overlay
        let source = get_source_for_overlay(&conn, "overlay-b").unwrap();
        assert_eq!(source, Some("source-pyramid".to_string()));

        // Non-overlay slug returns None
        let none = get_source_for_overlay(&conn, "source-pyramid").unwrap();
        assert_eq!(none, None);
    }

    #[test]
    fn test_signature_mismatch_prevents_overlay() {
        let conn = setup_test_db();

        // The source has code ingest records. Try to check with a conversation signature.
        let config = ingest::default_ingest_config();
        let conv_sig = ingest::ingest_signature(&ContentType::Conversation, &config);

        // Direct signature check should fail
        assert!(
            !check_ingest_signature_match(&conn, "source-pyramid", &conv_sig),
            "Conversation signature should not match code ingest records"
        );

        // create_overlay_build uses the source's content_type to compute sig,
        // so it will match. To test a genuine mismatch we need a source with
        // NO ingest records at all.
        db::create_slug(&conn, "empty-source", &ContentType::Code, "/empty").unwrap();
        let result = create_overlay_build(&conn, "empty-source", "overlay-x", "chain-x");
        assert!(result.is_err(), "Should fail when no ingest records match");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Ingest signature mismatch"),
            "Error should mention signature mismatch, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_remove_overlay_soft_deletes() {
        let conn = setup_test_db();
        let config = ingest::default_ingest_config();
        let sig = ingest::ingest_signature(&ContentType::Code, &config);

        add_chain_overlay(&conn, "source-pyramid", "to-remove", "chain-rm", &sig).unwrap();

        // Verify it exists
        let overlays = get_overlays_for_source(&conn, "source-pyramid").unwrap();
        assert_eq!(overlays.len(), 1);

        // Remove it
        let removed = remove_chain_overlay(&conn, "source-pyramid", "to-remove").unwrap();
        assert!(removed, "Should return true when an overlay was removed");

        // Active overlays should now be empty
        let overlays = get_overlays_for_source(&conn, "source-pyramid").unwrap();
        assert_eq!(overlays.len(), 0, "Removed overlay should not appear in active list");

        // But the row still exists in the DB (soft delete)
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_chain_overlays WHERE source_slug = 'source-pyramid'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "Row should still exist (soft delete)");

        // Removing again should return false (already removed)
        let removed_again = remove_chain_overlay(&conn, "source-pyramid", "to-remove").unwrap();
        assert!(!removed_again, "Should return false when overlay already removed");
    }
}
