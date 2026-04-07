//! OTP-bridged web session storage (post-agents-retro WS-E).
//!
//! Bridges Supabase OTP auth into a 7-day opaque server-side session token
//! stored in the local SQLite `web_sessions` table (schema landed in the
//! Phase 0.5 skeleton: see `pyramid::db::init_pyramid_db`).
//!
//! Pillar 1 note: web_sessions are ephemeral auth state — NOT contributions.
//! DELETE on logout / sweep is the correct semantics here.
//!
//! Pillar 13 note: `supabase_user_id` is a Supabase id, NEVER a Wire
//! `operator_id`. Do not pass it into anything that expects an operator_id.

use rusqlite::{Connection, OptionalExtension, Result as SqlResult, params};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone)]
pub struct WebSession {
    pub token: String,
    pub supabase_user_id: String,
    pub email: String,
    pub created_at: String,
    pub expires_at: String,
}

/// Insert a new session. Returns the random opaque token (256-bit hex).
pub fn create(
    conn: &Connection,
    supabase_user_id: &str,
    email: &str,
    ttl_seconds: i64,
) -> SqlResult<String> {
    let token = generate_token();
    // SQLite literal interpolation of an integer is safe; expires_at column
    // is a TEXT but we store the result of datetime(...) so it's compatible
    // with the lookup's `datetime(expires_at) > datetime('now')` predicate.
    // SQLite datetime modifier must be e.g. '+3600 seconds' or '-10 seconds';
    // formatting with a bare signed integer would yield '+-10 seconds' for
    // negative TTLs (used in tests) which sqlite rejects.
    let modifier = if ttl_seconds >= 0 {
        format!("+{} seconds", ttl_seconds)
    } else {
        format!("{} seconds", ttl_seconds)
    };
    let sql = format!(
        "INSERT INTO web_sessions (token, supabase_user_id, email, expires_at) \
         VALUES (?1, ?2, ?3, datetime('now', '{}'))",
        modifier
    );
    conn.execute(&sql, params![token, supabase_user_id, email])?;
    Ok(token)
}

/// Lookup a session by token, returning Some only if not expired.
pub fn lookup(conn: &Connection, token: &str) -> SqlResult<Option<WebSession>> {
    conn.query_row(
        "SELECT token, supabase_user_id, email, created_at, expires_at \
         FROM web_sessions \
         WHERE token = ?1 AND datetime(expires_at) > datetime('now')",
        params![token],
        |row| {
            Ok(WebSession {
                token: row.get(0)?,
                supabase_user_id: row.get(1)?,
                email: row.get(2)?,
                created_at: row.get(3)?,
                expires_at: row.get(4)?,
            })
        },
    )
    .optional()
}

/// Delete a single session (logout). Returns rows affected.
pub fn delete(conn: &Connection, token: &str) -> SqlResult<usize> {
    conn.execute(
        "DELETE FROM web_sessions WHERE token = ?1",
        params![token],
    )
}

/// Delete all expired sessions (sweeper).
pub fn sweep_expired(conn: &Connection) -> SqlResult<usize> {
    conn.execute(
        "DELETE FROM web_sessions WHERE datetime(expires_at) < datetime('now')",
        [],
    )
}

/// Generate a 256-bit opaque token, hex-encoded (64 chars).
///
/// Uses two v4 UUIDs concatenated for 32 bytes of randomness. This avoids
/// pulling in the `rand` crate (not currently a direct dependency).
fn generate_token() -> String {
    let a = *uuid::Uuid::new_v4().as_bytes();
    let b = *uuid::Uuid::new_v4().as_bytes();
    let mut buf = [0u8; 32];
    buf[..16].copy_from_slice(&a);
    buf[16..].copy_from_slice(&b);
    hex::encode(buf)
}

// ── Sweeper task ────────────────────────────────────────────────────────

static SWEEPER_STARTED: OnceLock<()> = OnceLock::new();

/// Spawn a background task that sweeps expired sessions every hour.
///
/// Idempotent: only the first call has any effect, even if called from
/// multiple init paths.
pub fn spawn_sweeper(state: Arc<crate::pyramid::PyramidState>) {
    if SWEEPER_STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
        // First tick fires immediately — skip it so we don't sweep an empty
        // table at boot.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let conn = state.writer.lock().await;
            match sweep_expired(&conn) {
                Ok(n) if n > 0 => {
                    tracing::info!("web_sessions sweeper: deleted {} expired", n);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("web_sessions sweeper: {}", e);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE web_sessions (
                token TEXT PRIMARY KEY,
                supabase_user_id TEXT NOT NULL,
                email TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn create_lookup_delete_roundtrip() {
        let conn = mem_db();
        let token = create(&conn, "user-abc", "alice@example.com", 3600).unwrap();
        assert_eq!(token.len(), 64);
        let s = lookup(&conn, &token).unwrap().expect("session present");
        assert_eq!(s.supabase_user_id, "user-abc");
        assert_eq!(s.email, "alice@example.com");
        let n = delete(&conn, &token).unwrap();
        assert_eq!(n, 1);
        assert!(lookup(&conn, &token).unwrap().is_none());
    }

    #[test]
    fn expired_sessions_not_returned_and_swept() {
        let conn = mem_db();
        // ttl = -10 → already expired
        let token = create(&conn, "u", "e@x.y", -10).unwrap();
        assert!(lookup(&conn, &token).unwrap().is_none());
        let swept = sweep_expired(&conn).unwrap();
        assert_eq!(swept, 1);
    }
}
