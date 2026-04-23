// pyramid/purpose.rs — Per-slug purpose declaration with supersession chain.
//
// Post-build accretion v5: every pyramid has a `Purpose` contribution that
// governs which meta-layers can crystallize. Stock purposes are auto-derived
// from ContentType for new slugs; operators may declare specific purposes via
// CLI/HTTP/Tauri surface.
//
// Supersession pattern: when purpose shifts, a new row is inserted with
// `superseded_by` pointing at the prior row. Partial UNIQUE index on
// `(slug) WHERE superseded_by IS NULL` enforces exactly one active purpose.
//
// All code paths that need the active purpose call `load_or_create_purpose`
// — never `load_purpose` in production. `load_or_create_purpose` auto-seeds
// a stock purpose from ContentType on first call for legacy slugs.

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::types::{ContentType, Purpose};

/// Map content_type → stock_purpose_key. Used by `initialize_from_content_type`
/// to seed a default purpose on slug creation.
///
/// None means no stock purpose applies (shape-specific pyramids are not
/// created by the wizard; they emerge via role handlers).
pub fn stock_purpose_for(content_type: &ContentType) -> (&'static str, &'static str) {
    match content_type {
        ContentType::Code => (
            "understand_codebase",
            "Understand this codebase and how it is organized.",
        ),
        ContentType::Conversation => (
            "understand_conversation",
            "Understand this conversation corpus and the ideas it contains.",
        ),
        ContentType::Document => (
            "understand_document_corpus",
            "Understand this document corpus and how it is organized.",
        ),
        ContentType::Vine => (
            "compose_vine",
            "Compose understanding across the vine's member pyramids.",
        ),
        ContentType::Question => (
            "answer_question",
            "Answer a specific question across the referenced pyramid substrate.",
        ),
    }
}

/// Declare a purpose for a slug. If an active purpose exists, caller should
/// use `supersede_purpose` instead — this function will fail the UNIQUE
/// constraint if one is already active.
pub fn declare_purpose(
    conn: &Connection,
    slug: &str,
    purpose_text: &str,
    stock_purpose_key: Option<&str>,
    decomposition_chain_ref: Option<&str>,
) -> Result<Purpose> {
    conn.execute(
        "INSERT INTO pyramid_purposes
            (slug, purpose_text, stock_purpose_key, decomposition_chain_ref)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            slug,
            purpose_text,
            stock_purpose_key,
            decomposition_chain_ref
        ],
    )
    .with_context(|| format!("Failed to declare purpose for slug '{slug}'"))?;

    load_purpose(conn, slug)?
        .ok_or_else(|| anyhow::anyhow!("purpose not found after insert for '{slug}'"))
}

/// Seed a stock purpose from ContentType. Idempotent: uses INSERT OR IGNORE
/// against the partial UNIQUE index so callers can invoke blindly from
/// `db::create_slug`.
pub fn initialize_from_content_type(
    conn: &Connection,
    slug: &str,
    content_type: &ContentType,
) -> Result<()> {
    let (stock_key, default_text) = stock_purpose_for(content_type);
    // INSERT OR IGNORE — the partial UNIQUE index rejects a second active
    // purpose for this slug if one already exists. No-op in that case.
    conn.execute(
        "INSERT OR IGNORE INTO pyramid_purposes
            (slug, purpose_text, stock_purpose_key)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![slug, default_text, stock_key],
    )
    .with_context(|| format!("Failed to seed stock purpose for slug '{slug}'"))?;
    Ok(())
}

/// Supersede the active purpose with a new declaration.
///
/// Supersession dance: the partial UNIQUE index
/// `WHERE superseded_by IS NULL` enforces exactly one active row per slug,
/// which collides with the naive INSERT-then-UPDATE pattern because the new
/// row's INSERT fires the UNIQUE check before the prior row is marked
/// superseded. Work around by first setting the prior row's superseded_by
/// to its own id (self-reference — FK-valid, partial-UNIQUE-exempt), then
/// inserting the new active row, then redirecting the prior row's pointer
/// to the successor's id.
pub fn supersede_purpose(
    conn: &Connection,
    slug: &str,
    new_purpose_text: &str,
    supersede_reason: Option<&str>,
    stock_purpose_key: Option<&str>,
    decomposition_chain_ref: Option<&str>,
) -> Result<Purpose> {
    // Find active purpose id (if any)
    let prior_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM pyramid_purposes WHERE slug = ?1 AND superseded_by IS NULL",
            rusqlite::params![slug],
            |row| row.get::<_, i64>(0),
        )
        .ok();

    // Step 1: park the prior row outside the active partial index via
    // self-reference so the new row can INSERT without UNIQUE collision.
    if let Some(pid) = prior_id {
        conn.execute(
            "UPDATE pyramid_purposes SET superseded_by = id WHERE id = ?1",
            rusqlite::params![pid],
        )
        .with_context(|| {
            format!("Failed to park prior purpose (self-ref) for slug '{slug}'")
        })?;
    }

    // Step 2: INSERT new active row
    conn.execute(
        "INSERT INTO pyramid_purposes
            (slug, purpose_text, stock_purpose_key, decomposition_chain_ref, supersede_reason)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            slug,
            new_purpose_text,
            stock_purpose_key,
            decomposition_chain_ref,
            supersede_reason
        ],
    )
    .with_context(|| format!("Failed to insert successor purpose for slug '{slug}'"))?;
    let new_id = conn.last_insert_rowid();

    // Step 3: redirect the prior row's pointer from self to the successor
    if let Some(pid) = prior_id {
        conn.execute(
            "UPDATE pyramid_purposes SET superseded_by = ?1 WHERE id = ?2",
            rusqlite::params![new_id, pid],
        )
        .with_context(|| format!("Failed to mark prior purpose superseded for slug '{slug}'"))?;
    }

    // Emit a `purpose_shifted` observation event so the DADBEAR compiler can
    // route it to the purpose-aware meta_layer_oracle role. Metadata carries
    // the prior id + stock key + reason so downstream chains can reason about
    // what shifted and why without a second DB query.
    //
    // Propagate (no `.ok()` swallow): the supersede dance already committed
    // three writes to pyramid_purposes above. If the observation write fails
    // here, the state change has landed but is invisible to downstream chains
    // — exactly the kind of silent divergence `feedback_loud_deferrals` is
    // about. Per v5 R5 loud-raise discipline, fail the whole call and let the
    // caller retry or surface the error. `dadbear_observation_events` is
    // created unconditionally by `init_pyramid_db`, so the earlier "tolerate
    // missing table in test harnesses" rationale doesn't apply.
    let metadata = serde_json::json!({
        "prior_purpose_id": prior_id,
        "new_purpose_id": new_id,
        "new_stock_purpose_key": stock_purpose_key,
        "supersede_reason": supersede_reason,
        "new_purpose_text_preview": new_purpose_text.chars().take(200).collect::<String>(),
    })
    .to_string();
    super::observation_events::write_observation_event(
        conn,
        slug,
        "purpose",         // source
        "purpose_shifted", // event_type
        None,              // source_path
        None,              // file_path
        None,              // content_hash
        None,              // previous_hash
        None,              // target_node_id — purpose is slug-level, not node-level
        None,              // layer
        Some(&metadata),
    )
    .with_context(|| {
        format!("Failed to emit purpose_shifted observation event for slug '{slug}'")
    })?;

    load_purpose(conn, slug)?.ok_or_else(|| {
        anyhow::anyhow!("successor purpose not found after supersede for '{slug}'")
    })
}

/// Load the active (non-superseded) purpose for a slug.
/// Returns None if the slug has never had a purpose declared.
pub fn load_purpose(conn: &Connection, slug: &str) -> Result<Option<Purpose>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, purpose_text, stock_purpose_key, decomposition_chain_ref,
                created_at, superseded_by, supersede_reason
           FROM pyramid_purposes
          WHERE slug = ?1 AND superseded_by IS NULL
          LIMIT 1",
    )?;
    let result = stmt.query_row(rusqlite::params![slug], |row| {
        Ok(Purpose {
            id: row.get(0)?,
            slug: row.get(1)?,
            purpose_text: row.get(2)?,
            stock_purpose_key: row.get(3)?,
            decomposition_chain_ref: row.get(4)?,
            created_at: row.get(5)?,
            superseded_by: row.get(6)?,
            supersede_reason: row.get(7)?,
        })
    });
    match result {
        Ok(p) => Ok(Some(p)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to load purpose for slug '{slug}'")),
    }
}

/// Load the active purpose, creating a stock one from the slug's content_type
/// if none exists. Use this in code paths that need a purpose and are willing
/// to tolerate a synchronous self-heal insert.
pub fn load_or_create_purpose(conn: &Connection, slug: &str) -> Result<Purpose> {
    if let Some(p) = load_purpose(conn, slug)? {
        return Ok(p);
    }
    // Read content_type to derive stock purpose
    let ct_str: String = conn
        .query_row(
            "SELECT content_type FROM pyramid_slugs WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .with_context(|| format!("Slug '{slug}' not found when creating purpose"))?;
    let content_type = ContentType::from_str(&ct_str).unwrap_or(ContentType::Document);
    initialize_from_content_type(conn, slug, &content_type)?;
    load_purpose(conn, slug)?
        .ok_or_else(|| anyhow::anyhow!("purpose still missing for '{slug}' after init"))
}

/// List the full supersession chain for a slug (newest first).
pub fn list_purpose_supersession(conn: &Connection, slug: &str) -> Result<Vec<Purpose>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, purpose_text, stock_purpose_key, decomposition_chain_ref,
                created_at, superseded_by, supersede_reason
           FROM pyramid_purposes
          WHERE slug = ?1
          ORDER BY id DESC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![slug], |row| {
            Ok(Purpose {
                id: row.get(0)?,
                slug: row.get(1)?,
                purpose_text: row.get(2)?,
                stock_purpose_key: row.get(3)?,
                decomposition_chain_ref: row.get(4)?,
                created_at: row.get(5)?,
                superseded_by: row.get(6)?,
                supersede_reason: row.get(7)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
