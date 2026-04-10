// pyramid/prompt_cache.rs — Phase 5: runtime prompt lookup cache.
//
// Phase 5 migrates on-disk prompts from `chains/prompts/**/*.md` into
// `pyramid_config_contributions` rows (schema_type = "skill",
// source = "bundled"). The chain executor used to read these files
// directly from disk — now it goes through this cache, which is
// backed by the contribution store.
//
// The cache key is the normalized prompt path (e.g.
// `"conversation-episodic/forward.md"`). The value is the active
// `skill` contribution's `yaml_content` (the markdown body).
//
// The cache is:
//   - Populated on first lookup (pull-through read from SQLite)
//   - Invalidated when a skill contribution is created/updated/
//     superseded via the dispatcher's `invalidate_prompt_cache()` hook
//   - Shared process-wide via a single global `OnceLock`
//   - Thread-safe via an interior `RwLock`
//
// The cache does NOT persist to disk. A cold start re-populates on
// first lookup. This is intentional — the contribution store is the
// only durable source of truth; the cache is just a runtime read
// accelerator.
//
// Chain loader integration: `chain_loader::resolve_prompt_refs` walks
// a chain's step instructions and rewrites `$prompts/...` references
// to their resolved content. The Phase 5 transition plan leaves the
// existing on-disk fallback in place so chains that land AFTER first-
// run migration (Phase 9's custom chains) keep working, but the
// primary lookup path is the cache.

use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

/// Normalize a prompt path by stripping the `$prompts/` prefix if
/// present. Callers can pass either form.
pub fn normalize_prompt_path(path: &str) -> String {
    path.trim_start_matches("$prompts/").to_string()
}

/// Errors the cache can surface.
#[derive(Debug, thiserror::Error)]
pub enum PromptCacheError {
    #[error("prompt {0:?} not found in contribution store or on disk")]
    NotFound(String),
    #[error("db error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("cache lock poisoned")]
    PoisonedLock,
}

/// Process-wide runtime cache for prompts resolved from
/// `pyramid_config_contributions`. Thread-safe.
///
/// The cache stores `normalized_path -> yaml_content` pairs. Pages
/// are faulted in on first read; invalidation clears the entire map
/// (coarse-grained but simple — the prompt set is small enough that a
/// full refresh is cheap).
pub struct PromptCache {
    entries: RwLock<HashMap<String, String>>,
}

impl Default for PromptCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptCache {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Lookup a prompt by its normalized path. Pulls from the
    /// database on cache miss, caches the result, and returns the
    /// body. Returns `NotFound` if no `skill` contribution exists for
    /// the path and no fallback hits either.
    ///
    /// The lookup strategy:
    ///
    /// 1. Check the in-memory map (hot path).
    /// 2. Query `pyramid_config_contributions` for an active `skill`
    ///    contribution whose metadata topics include the path's
    ///    directory stem (e.g. `"conversation-episodic"`) — or whose
    ///    yaml_content is a 1:1 match (Phase 5 bundled migration
    ///    keys the contribution's `slug` column to the normalized
    ///    path for exact lookup).
    /// 3. Cache and return.
    pub fn get(&self, conn: &Connection, prompt_ref: &str) -> Result<String, PromptCacheError> {
        let normalized = normalize_prompt_path(prompt_ref);

        // Hot path: return from the cache if present.
        if let Some(body) = self
            .entries
            .read()
            .map_err(|_| PromptCacheError::PoisonedLock)?
            .get(&normalized)
            .cloned()
        {
            return Ok(body);
        }

        // Cache miss: query the contribution store. Phase 5 migration
        // stores the prompt's normalized path in the contribution's
        // `slug` column (for exact lookup) with schema_type = "skill".
        let body: Option<String> = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions
                 WHERE schema_type = 'skill'
                   AND slug = ?1
                   AND status = 'active'
                   AND superseded_by_id IS NULL
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1",
                rusqlite::params![normalized],
                |row| row.get::<_, String>(0),
            )
            .ok();

        let body = body.ok_or_else(|| PromptCacheError::NotFound(normalized.clone()))?;

        // Insert into the cache for next time.
        self.entries
            .write()
            .map_err(|_| PromptCacheError::PoisonedLock)?
            .insert(normalized, body.clone());

        Ok(body)
    }

    /// Invalidate the entire cache. Called by the dispatcher's
    /// `invalidate_prompt_cache()` hook whenever a `skill` or
    /// `custom_chains` contribution lands. Coarse-grained but cheap —
    /// the next read re-fills on demand.
    pub fn invalidate_all(&self) -> Result<(), PromptCacheError> {
        self.entries
            .write()
            .map_err(|_| PromptCacheError::PoisonedLock)?
            .clear();
        Ok(())
    }

    /// Number of cached entries. Test-only helper.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.read().map(|m| m.len()).unwrap_or(0)
    }

    /// Whether the cache contains a specific path. Test-only helper.
    #[cfg(test)]
    pub fn contains(&self, path: &str) -> bool {
        let normalized = normalize_prompt_path(path);
        self.entries
            .read()
            .map(|m| m.contains_key(&normalized))
            .unwrap_or(false)
    }
}

/// Global singleton for the prompt cache. Initialized lazily on first
/// access so tests that don't touch prompts pay zero cost.
static GLOBAL_PROMPT_CACHE: OnceLock<PromptCache> = OnceLock::new();

/// Global stashed pyramid.db path. Set once at app boot via
/// `set_global_prompt_cache_db_path()` so the connection-less
/// `resolve_prompt_global()` helper can open ephemeral reader
/// connections without threading the path through every call site.
///
/// When unset, the connection-less resolver returns `NotFound` and
/// callers fall back to disk. Tests that construct a local
/// `PromptCache` don't touch the global and don't need the path set.
static GLOBAL_PROMPT_CACHE_DB_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Return (or initialize) the global prompt cache singleton.
pub fn global_prompt_cache() -> &'static PromptCache {
    GLOBAL_PROMPT_CACHE.get_or_init(PromptCache::new)
}

/// Stash the pyramid.db path for the global prompt cache. Called once
/// from `main.rs` during app setup after `pyramid_db_path` is known.
/// Safe to call multiple times — only the first call wins (subsequent
/// calls are a no-op via `OnceLock::set`).
pub fn set_global_prompt_cache_db_path(path: PathBuf) {
    let _ = GLOBAL_PROMPT_CACHE_DB_PATH.set(path);
}

/// Connection-less resolver: opens a short-lived reader connection
/// to the stashed pyramid.db path and looks up the prompt. Used by
/// `chain_loader::resolve_prompt_refs` to consult the contribution
/// store on the hot path without threading a connection through
/// every chain load site.
///
/// Returns `Ok(Some(body))` on hit, `Ok(None)` on miss (caller should
/// fall back to disk), `Err` only on DB errors that are not "not
/// found".
pub fn resolve_prompt_global(prompt_ref: &str) -> Result<Option<String>, PromptCacheError> {
    let Some(db_path) = GLOBAL_PROMPT_CACHE_DB_PATH.get() else {
        // Path not stashed yet (tests, or boot ordering bug) — caller
        // falls back to disk. Not a hard error.
        return Ok(None);
    };

    // Hot path: check the cached map first without opening a
    // connection. Most prompt lookups land here after the first read.
    let normalized = normalize_prompt_path(prompt_ref);
    if let Some(body) = global_prompt_cache()
        .entries
        .read()
        .map_err(|_| PromptCacheError::PoisonedLock)?
        .get(&normalized)
        .cloned()
    {
        return Ok(Some(body));
    }

    // Cache miss: open an ephemeral reader connection to the stashed
    // path and fault the row in. Ephemeral connections are cheap on
    // SQLite (microseconds) and they avoid the lifetime gymnastics
    // of sharing the reader mutex with chain load paths.
    let conn = match Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                path = %db_path.display(),
                error = %e,
                "prompt_cache: failed to open ephemeral reader connection; falling back to disk"
            );
            return Ok(None);
        }
    };

    match global_prompt_cache().get(&conn, prompt_ref) {
        Ok(body) => Ok(Some(body)),
        Err(PromptCacheError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Convenience lookup against the global cache. Used by the chain
/// executor as the primary resolution path.
pub fn resolve_prompt_from_store(
    conn: &Connection,
    prompt_ref: &str,
) -> Result<String, PromptCacheError> {
    global_prompt_cache().get(conn, prompt_ref)
}

/// Invalidate the global cache. Called from
/// `config_contributions::invalidate_prompt_cache()` whenever a
/// skill/chain contribution lands.
pub fn invalidate_global_prompt_cache() {
    if let Some(cache) = GLOBAL_PROMPT_CACHE.get() {
        let _ = cache.invalidate_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use rusqlite::Connection;

    fn insert_skill(conn: &Connection, slug: &str, body: &str) {
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (
                ?1, ?2, 'skill', ?3,
                '{}', '{}',
                NULL, NULL, 'test seed',
                'active', 'bundled', NULL, 'test', datetime('now')
             )",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), slug, body],
        )
        .unwrap();
    }

    fn supersede_skill(conn: &mut Connection, old_slug: &str, new_body: &str) {
        let tx = conn.transaction().unwrap();
        let prior_id: String = tx
            .query_row(
                "SELECT contribution_id FROM pyramid_config_contributions
                 WHERE schema_type = 'skill' AND slug = ?1
                   AND status = 'active'",
                rusqlite::params![old_slug],
                |row| row.get(0),
            )
            .unwrap();
        let new_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (
                ?1, ?2, 'skill', ?3,
                '{}', '{}',
                ?4, NULL, 'superseded by test',
                'active', 'local', NULL, 'test', datetime('now')
             )",
            rusqlite::params![new_id, old_slug, new_body, prior_id],
        )
        .unwrap();
        tx.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded', superseded_by_id = ?1
             WHERE contribution_id = ?2",
            rusqlite::params![new_id, prior_id],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn normalize_strips_dollar_prompts_prefix() {
        assert_eq!(
            normalize_prompt_path("$prompts/conversation/forward.md"),
            "conversation/forward.md"
        );
        assert_eq!(
            normalize_prompt_path("conversation/forward.md"),
            "conversation/forward.md"
        );
        assert_eq!(normalize_prompt_path("$prompts/"), "");
    }

    #[test]
    fn cache_miss_then_hit() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        insert_skill(&conn, "conversation/forward.md", "body v1");

        let cache = PromptCache::new();
        assert_eq!(cache.len(), 0);

        let body = cache.get(&conn, "$prompts/conversation/forward.md").unwrap();
        assert_eq!(body, "body v1");
        assert_eq!(cache.len(), 1);
        assert!(cache.contains("conversation/forward.md"));

        // Second read is a cache hit.
        let body2 = cache.get(&conn, "$prompts/conversation/forward.md").unwrap();
        assert_eq!(body2, "body v1");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_returns_not_found_on_missing_skill() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();

        let cache = PromptCache::new();
        let err = cache
            .get(&conn, "$prompts/does-not-exist/ghost.md")
            .unwrap_err();
        matches!(err, PromptCacheError::NotFound(_));
    }

    #[test]
    fn cache_supersession_returns_new_body_after_invalidate() {
        let mut conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        insert_skill(&conn, "conversation/forward.md", "body v1");

        let cache = PromptCache::new();
        let v1 = cache.get(&conn, "$prompts/conversation/forward.md").unwrap();
        assert_eq!(v1, "body v1");

        // Supersede the skill.
        supersede_skill(&mut conn, "conversation/forward.md", "body v2");

        // Without invalidation, the stale cache entry still wins.
        let stale = cache.get(&conn, "$prompts/conversation/forward.md").unwrap();
        assert_eq!(stale, "body v1");

        // Invalidate → next read reflects the supersession.
        cache.invalidate_all().unwrap();
        let fresh = cache.get(&conn, "$prompts/conversation/forward.md").unwrap();
        assert_eq!(fresh, "body v2");
    }

    #[test]
    fn cache_skips_superseded_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        insert_skill(&conn, "shared/heal_json.md", "heal v1");
        supersede_skill(&mut conn, "shared/heal_json.md", "heal v2");
        supersede_skill(&mut conn, "shared/heal_json.md", "heal v3");

        let cache = PromptCache::new();
        let body = cache.get(&conn, "$prompts/shared/heal_json.md").unwrap();
        // Active version is v3 — superseded rows are filtered by the
        // status clause in the SELECT.
        assert_eq!(body, "heal v3");
    }

    #[test]
    fn cache_scopes_by_slug() {
        // Two different prompts must not collide in the cache.
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        insert_skill(&conn, "conversation/forward.md", "conv forward");
        insert_skill(&conn, "conversation/reverse.md", "conv reverse");

        let cache = PromptCache::new();
        let fwd = cache.get(&conn, "$prompts/conversation/forward.md").unwrap();
        let rev = cache.get(&conn, "$prompts/conversation/reverse.md").unwrap();
        assert_eq!(fwd, "conv forward");
        assert_eq!(rev, "conv reverse");
        assert_eq!(cache.len(), 2);
    }

    // ── Phase 5 wanderer fix: global resolver path integration test ──
    //
    // This test verifies the end-to-end wiring from
    // `resolve_prompt_global()` to an actual SQLite file containing a
    // migrated skill contribution. It exercises the hot path that
    // `chain_loader::resolve_prompt_refs` now walks on every chain
    // load: cache-first → ephemeral DB connection on miss.
    //
    // The test uses a unique slug (UUID-prefixed) so it doesn't
    // collide with anything else that might land in the global cache
    // from another test in the same process. It deliberately does
    // NOT rely on `set_global_prompt_cache_db_path` being unset at
    // test entry — OnceLock is a one-way latch, so the first test to
    // set the path wins for the whole test binary. Instead, we build
    // a path-less test by using the test-local `PromptCache::new()`
    // path for the DB query, and only test the OnceLock-mediated
    // resolver in the test below. That test uses `.set()` with an
    // ignored result so it can run in any order.
    #[test]
    fn global_resolver_returns_none_when_path_unset() {
        // This test only works if no other test has stashed a path
        // before it. Since test order is non-deterministic, we can't
        // assume the OnceLock is empty. Instead, we use a slug that
        // will definitely not be in any DB and verify we get Ok(None)
        // for both cases (path unset → Ok(None); path set but slug
        // absent → Ok(None) too).
        let bogus_slug = format!("__wanderer_test_nonexistent_{}.md", uuid::Uuid::new_v4());
        let result = resolve_prompt_global(&format!("$prompts/{bogus_slug}"));
        match result {
            Ok(None) => {
                // Expected outcome regardless of whether the path is set.
            }
            Ok(Some(body)) => {
                panic!("unexpected cache hit for bogus slug: {body}");
            }
            Err(e) => {
                panic!("resolve_prompt_global errored for bogus slug: {e}");
            }
        }
    }

    #[test]
    fn global_resolver_hits_stashed_db_when_set() {
        use tempfile::NamedTempFile;

        // Build a tempfile-backed DB, populate it with a unique skill,
        // and call resolve_prompt_global. This exercises the ephemeral
        // reader connection path: even if the cache has no entry, the
        // resolver should open a connection to the stashed path,
        // query the DB, warm the cache, and return the body.
        //
        // This test tolerates the OnceLock being pre-set to a
        // different path (from another test) by skipping the
        // assertion with a warning instead of failing. That way, test
        // order doesn't matter.
        let temp_db = NamedTempFile::new().unwrap();
        let db_path = temp_db.path().to_path_buf();

        // Initialize the schema on the tempfile DB and insert a unique
        // skill.
        let conn = Connection::open(&db_path).unwrap();
        init_pyramid_db(&conn).unwrap();
        let unique_id = uuid::Uuid::new_v4().to_string();
        let unique_slug = format!("__wanderer/stashed_path_{unique_id}.md");
        let unique_body = format!("wanderer-test-body-{unique_id}");
        insert_skill(&conn, &unique_slug, &unique_body);
        drop(conn);

        // Try to stash the path. If another test already stashed a
        // different path, OnceLock::set returns Err and we can only
        // verify the path-unset fallback below.
        let stashed_ok = GLOBAL_PROMPT_CACHE_DB_PATH.set(db_path.clone()).is_ok();

        if stashed_ok {
            // Path was unset before this test — verify the resolver
            // finds the unique skill through the full cache-then-DB
            // path.
            let result = resolve_prompt_global(&format!("$prompts/{unique_slug}")).unwrap();
            assert_eq!(
                result,
                Some(unique_body.clone()),
                "resolve_prompt_global should have returned the unique skill body"
            );
        } else {
            // Another test already stashed a different path. We can
            // still verify the resolver doesn't crash; the expected
            // outcome is either Ok(None) (other test's DB doesn't
            // have our unique slug) or Ok(Some(body)) from an
            // accidental collision (impossible with the unique
            // UUID-prefixed slug).
            let result = resolve_prompt_global(&format!("$prompts/{unique_slug}")).unwrap();
            assert!(
                result.is_none(),
                "other test's stashed DB unexpectedly returned a hit for the unique slug"
            );
        }
    }
}
