//! Reserved subpath check for `/p/{slug}/{node_id}` routing.
//!
//! Per A9 in the post-agents-retro plan: routes that are not pyramid node IDs
//! either start with `_` (e.g. `_login`, `_ws`, `_assets`) or are one of a
//! small set of reserved keywords (`tree`, `search`, `glossary`, `folio`).
//! When the catchall `/p/{slug}/{node_id}` handler runs, it must reject these
//! so the dedicated handlers (mounted earlier in the filter chain) can serve
//! them — and so a node accidentally named `tree` cannot shadow the tree page.

pub const RESERVED_SUBPATHS: &[&str] = &["tree", "search", "glossary", "folio"];

/// Returns true when `s` would collide with a non-node-id subpath under
/// `/p/{slug}/...`. Match rules:
/// - Anything starting with `_` (the underscore namespace).
/// - Exact match against any of `RESERVED_SUBPATHS`.
pub fn is_reserved_subpath(s: &str) -> bool {
    s.starts_with('_') || RESERVED_SUBPATHS.contains(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underscore_prefix_is_reserved() {
        assert!(is_reserved_subpath("_login"));
        assert!(is_reserved_subpath("_ws"));
        assert!(is_reserved_subpath("_"));
    }

    #[test]
    fn keywords_are_reserved() {
        for kw in RESERVED_SUBPATHS {
            assert!(is_reserved_subpath(kw));
        }
    }

    #[test]
    fn ordinary_node_ids_are_not_reserved() {
        assert!(!is_reserved_subpath("L0-001"));
        assert!(!is_reserved_subpath("apex"));
        assert!(!is_reserved_subpath("trees")); // not exact match
    }
}
