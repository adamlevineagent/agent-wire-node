// pyramid/slug.rs — Slug/namespace management with validation
//
// Thin wrappers around db:: functions that handle:
// - Slug name validation and normalization (slugify)
// - Existence checks before create/delete
// - Content type validation

use anyhow::{anyhow, Result};
use rusqlite::Connection;

use super::db;
use super::types::*;

/// Normalize a string into a valid slug: lowercase, alphanumeric + hyphens,
/// no leading/trailing hyphens, no consecutive hyphens.
fn slugify(input: &str) -> String {
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
fn validate_slug(slug: &str) -> Result<()> {
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

    // Check for duplicates
    if db::get_slug(conn, &normalized)?.is_some() {
        return Err(anyhow!("Slug '{}' already exists", normalized));
    }

    db::create_slug(conn, &normalized, content_type, source_path)
}

/// List all slugs.
pub fn list_slugs(conn: &Connection) -> Result<Vec<SlugInfo>> {
    db::list_slugs(conn)
}

/// Delete a slug and all associated data. Verifies the slug exists first.
pub fn delete_slug(conn: &Connection, slug: &str) -> Result<()> {
    if db::get_slug(conn, slug)?.is_none() {
        return Err(anyhow!("Slug '{}' not found", slug));
    }
    db::delete_slug(conn, slug)
}

/// Get a single slug by name.
pub fn get_slug(conn: &Connection, slug: &str) -> Result<Option<SlugInfo>> {
    db::get_slug(conn, slug)
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
    fn test_delete_checks_existence() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        let result = delete_slug(&conn, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
