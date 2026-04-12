// pyramid/slug.rs — Slug/namespace management with validation
//
// Thin wrappers around db:: functions that handle:
// - Slug name validation and normalization (slugify)
// - Existence checks before create/delete
// - Content type validation

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

use super::db;
use super::types::*;

/// Normalize a string into a valid slug: lowercase, alphanumeric + hyphens,
/// no leading/trailing hyphens, no consecutive hyphens.
pub fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());

    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' || ch == ' ' || ch == '.' {
            // Collapse separators into a single hyphen
            if !slug.ends_with('-') {
                slug.push('-');
            }
        }
    }

    // Trim leading/trailing hyphens
    slug.trim_matches('-').to_string()
}

/// Validate that a slug name is acceptable (non-empty, reasonable length).
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        return Err(anyhow!("Slug cannot be empty"));
    }
    if slug.len() > 128 {
        return Err(anyhow!("Slug too long (max 128 characters)"));
    }
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(anyhow!(
            "Slug contains invalid characters (only a-z, 0-9, hyphen allowed)"
        ));
    }
    Ok(())
}

/// Create a new slug. Normalizes the name, checks for duplicates, and inserts.
pub fn create_slug(
    conn: &Connection,
    slug: &str,
    content_type: &ContentType,
    source_path: &str,
) -> Result<SlugInfo> {
    let normalized = slugify(slug);
    validate_slug(&normalized)?;

    // Check for duplicates — archived slugs still block the name to avoid
    // carrying stale nodes, old build IDs, and orphaned data
    if let Some(existing) = db::get_slug(conn, &normalized)? {
        if existing.archived_at.is_some() {
            return Err(anyhow!(
                "Slug '{}' was previously used and archived. Choose a different name.",
                normalized
            ));
        }
        return Err(anyhow!("Slug '{}' already exists", normalized));
    }

    db::create_slug(conn, &normalized, content_type, source_path)
}

fn parse_source_paths(source_path: &str) -> Result<Vec<String>> {
    let trimmed = source_path.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("source_path cannot be empty"));
    }

    if trimmed.starts_with('[') {
        let parsed: Vec<String> = serde_json::from_str(trimmed)
            .map_err(|e| anyhow!("Invalid source_path array JSON: {e}"))?;
        if parsed.is_empty() {
            return Err(anyhow!("source_path array cannot be empty"));
        }
        return Ok(parsed);
    }

    Ok(vec![trimmed.to_string()])
}

fn is_sensitive_source_path(path: &Path, data_dir: Option<&Path>) -> bool {
    if let Some(data_dir) = data_dir {
        if path.starts_with(data_dir) {
            return true;
        }
    }

    let sensitive_segments = [
        ".ssh", ".aws", ".gnupg", ".config", "Library", "AppData", ".local",
    ];

    path.components().any(|component| {
        let text = component.as_os_str().to_string_lossy();
        sensitive_segments.iter().any(|segment| text == *segment)
    })
}

fn allowed_source_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(canonical) = cwd.canonicalize() {
            roots.push(canonical);
        }
    }
    if let Some(home) = dirs::home_dir() {
        if let Ok(canonical) = home.canonicalize() {
            roots.push(canonical);
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

pub fn resolve_validated_source_paths(
    source_path: &str,
    content_type: &ContentType,
    data_dir: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    // Question pyramids derive from other pyramids, not filesystem paths.
    if matches!(content_type, ContentType::Question) {
        return Ok(vec![]);
    }

    let allowed_roots = allowed_source_roots();
    if allowed_roots.is_empty() {
        return Err(anyhow!("No allowed source roots are configured"));
    }

    let parsed_paths = parse_source_paths(source_path)?;
    let mut validated = Vec::with_capacity(parsed_paths.len());

    for raw_path in parsed_paths {
        let candidate = PathBuf::from(raw_path.trim());
        if candidate.as_os_str().is_empty() {
            return Err(anyhow!("source_path entries cannot be empty"));
        }
        let canonical = candidate
            .canonicalize()
            .map_err(|e| anyhow!("Invalid source path '{}': {e}", candidate.display()))?;

        if !allowed_roots.iter().any(|root| canonical.starts_with(root)) {
            return Err(anyhow!(
                "Source path '{}' is outside the allowed roots",
                canonical.display()
            ));
        }
        if is_sensitive_source_path(&canonical, data_dir) {
            return Err(anyhow!(
                "Source path '{}' points to a restricted location",
                canonical.display()
            ));
        }

        match content_type {
            ContentType::Code | ContentType::Document | ContentType::Vine => {
                if !canonical.is_dir() {
                    return Err(anyhow!(
                        "Source path '{}' must be a directory for {} slugs",
                        canonical.display(),
                        content_type.as_str()
                    ));
                }
            }
            ContentType::Conversation => {
                if !canonical.is_file() {
                    return Err(anyhow!(
                        "Source path '{}' must be a file for conversation slugs",
                        canonical.display()
                    ));
                }
            }
            ContentType::Question => {
                // Question pyramids derive from other pyramids, not filesystem paths.
                // Source path validation is a no-op.
            }
        }

        validated.push(canonical);
    }

    Ok(validated)
}

pub fn normalize_and_validate_source_path(
    source_path: &str,
    content_type: &ContentType,
    data_dir: Option<&Path>,
) -> Result<String> {
    // Question pyramids derive from other pyramids, not filesystem paths.
    if matches!(content_type, ContentType::Question) {
        return Ok(String::new());
    }

    let paths = resolve_validated_source_paths(source_path, content_type, data_dir)?;
    let normalized: Vec<String> = paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();

    if normalized.len() == 1 && !source_path.trim_start().starts_with('[') {
        Ok(normalized[0].clone())
    } else {
        serde_json::to_string(&normalized)
            .map_err(|e| anyhow!("Failed to serialize source paths: {e}"))
    }
}

/// List all slugs.
pub fn list_slugs(conn: &Connection) -> Result<Vec<SlugInfo>> {
    db::list_slugs(conn)
}

/// Archive a slug (soft-delete). Sets `archived_at` timestamp. Verifies the slug exists first.
///
/// Accepts an optional event bus so the freeze routes through `auto_update_ops`
/// (ghost-engine contract). Callers with access to the bus should pass `Some(&bus)`.
pub fn archive_slug(
    conn: &Connection,
    slug: &str,
    event_bus: Option<&std::sync::Arc<crate::pyramid::event_bus::BuildEventBus>>,
) -> Result<()> {
    if db::get_slug(conn, slug)?.is_none() {
        return Err(anyhow!("Slug '{}' not found", slug));
    }
    db::archive_slug(conn, slug, event_bus)
}

/// Admin-only hard delete of a slug and all associated data. Verifies the slug exists first.
pub fn purge_slug(conn: &Connection, slug: &str) -> Result<()> {
    if db::get_slug(conn, slug)?.is_none() {
        return Err(anyhow!("Slug '{}' not found", slug));
    }
    db::purge_slug(conn, slug)
}

/// Get a single slug by name.
pub fn get_slug(conn: &Connection, slug: &str) -> Result<Option<SlugInfo>> {
    db::get_slug(conn, slug)
}

// ── WS-ONLINE-D: Pinning ────────────────────────────────────────────────────

/// Pin a remote pyramid: creates a pinned slug entry with source tunnel URL
/// and inserts all exported nodes into local SQLite.
///
/// If the slug already exists, it updates the pinned flag and tunnel URL,
/// then upserts all nodes. This handles both first-pin and re-pin (refresh).
pub fn pin_remote_pyramid(
    conn: &Connection,
    slug: &str,
    tunnel_url: &str,
    nodes: &[PyramidNode],
) -> Result<usize> {
    let normalized = slugify(slug);
    validate_slug(&normalized)?;

    // Set pinned flag and tunnel URL (creates slug if needed)
    db::pin_pyramid(conn, &normalized, tunnel_url)?;

    // Upsert all nodes from the export
    let count = db::upsert_pinned_nodes(conn, &normalized, nodes)?;

    tracing::info!(
        slug = %normalized,
        tunnel_url = %tunnel_url,
        node_count = count,
        "pinned remote pyramid"
    );

    Ok(count)
}

/// Unpin a pyramid: clears the pinned flag and source_tunnel_url.
/// NEVER deletes node data (Pillar 1).
pub fn unpin_pyramid(conn: &Connection, slug: &str) -> Result<()> {
    if db::get_slug(conn, slug)?.is_none() {
        return Err(anyhow!("Slug '{}' not found", slug));
    }
    db::unpin_pyramid(conn, slug)?;
    tracing::info!(slug = %slug, "unpinned pyramid (data preserved)");
    Ok(())
}

/// Check whether a slug is pinned.
pub fn is_pinned(conn: &Connection, slug: &str) -> Result<bool> {
    db::is_pinned(conn, slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("My Project"), "my-project");
        assert_eq!(slugify("hello_world.v2"), "hello-world-v2");
        assert_eq!(slugify("--leading--"), "leading");
        assert_eq!(slugify("UPPER CASE"), "upper-case");
        assert_eq!(slugify("a  b  c"), "a-b-c");
    }

    #[test]
    fn test_validate_slug() {
        assert!(validate_slug("good-slug").is_ok());
        assert!(validate_slug("abc123").is_ok());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("bad slug!").is_err());
        let long = "a".repeat(129);
        assert!(validate_slug(&long).is_err());
    }

    #[test]
    fn test_create_checks_duplicates() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        create_slug(&conn, "test", &ContentType::Code, "/src").unwrap();
        let result = create_slug(&conn, "test", &ContentType::Code, "/src");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn test_archive_checks_existence() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        let result = archive_slug(&conn, "nonexistent", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_purge_checks_existence() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        let result = purge_slug(&conn, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
