// partner/ — Partner Context System (Dennis)
//
// Modules:
//   context       — Context window assembly (8-section layout)
//   conversation  — Message handler, LLM calls, buffer management
//   routes        — Warp HTTP route handlers

pub mod context;
pub mod conversation;
pub mod routes;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::pyramid::PyramidState;

// ── Types ───────────────────────────────────────────────────────────

/// Dennis's cognitive state — reflected in the avatar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DennisState {
    Idle,
    Listening,
    Thinking,
    Crystallizing,
    Searching,
    Speaking,
    Error(String),
}

impl Default for DennisState {
    fn default() -> Self {
        DennisState::Idle
    }
}

/// A single message in the conversation buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: String,
    pub token_estimate: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Partner,
}

/// A topic summary from the warm layer (session-level L1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTopic {
    pub summary: String,
    pub created_at: String,
}

/// A lifted result from a mid-turn pyramid query (moved from section 7 to section 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiftedResult {
    pub query: String,
    pub result: String,
    pub node_ids: Vec<String>,
}

/// A partner conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// NULL for lobby sessions, slug name for topic threads.
    pub slug: Option<String>,
    pub is_lobby: bool,
    pub conversation_buffer: Vec<Message>,
    pub session_topics: Vec<SessionTopic>,
    pub hydrated_node_ids: Vec<String>,
    pub lifted_results: Vec<LiftedResult>,
    pub dennis_state: DennisState,
    pub warm_cursor: usize,
    pub created_at: String,
    pub last_active_at: String,
}

/// LLM configuration for the partner model.
#[derive(Debug, Clone)]
pub struct PartnerLlmConfig {
    pub api_key: String,
    pub partner_model: String,
}

/// Response returned from handle_message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartnerResponse {
    pub message: String,
    pub dennis_state: DennisState,
    pub brain_state: BrainState,
    pub session_id: String,
}

/// Brain state for the Space tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainState {
    pub hydrated_node_ids: Vec<String>,
    pub session_topics: Vec<SessionTopic>,
    pub lifted_results: Vec<LiftedResult>,
    pub buffer_tokens: usize,
    pub buffer_capacity: usize,
}

// ── State ───────────────────────────────────────────────────────────

/// Shared state for the partner system.
pub struct PartnerState {
    /// In-memory session cache (session_id -> Session).
    pub sessions: Mutex<HashMap<String, Session>>,
    /// Reference to the pyramid engine state.
    pub pyramid: Arc<PyramidState>,
    /// Own reader connection to pyramid.db (avoids contention with pyramid routes).
    pub pyramid_reader: Arc<Mutex<Connection>>,
    /// Writer connection to partner.db.
    pub partner_db: Arc<Mutex<Connection>>,
    /// Partner LLM config (model, API key).
    pub llm_config: tokio::sync::RwLock<PartnerLlmConfig>,
}

// ── Database ────────────────────────────────────────────────────────

/// Open (or create) the partner SQLite database at the given path.
pub fn open_partner_db(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    init_partner_db(&conn)?;
    Ok(conn)
}

/// Initialize partner tables. Call on app startup.
pub fn init_partner_db(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS partner_sessions (
            id TEXT PRIMARY KEY,
            slug TEXT,
            is_lobby INTEGER NOT NULL DEFAULT 0,
            conversation_buffer TEXT NOT NULL DEFAULT '[]',
            session_topics TEXT NOT NULL DEFAULT '[]',
            hydrated_node_ids TEXT NOT NULL DEFAULT '[]',
            lifted_results TEXT NOT NULL DEFAULT '[]',
            dennis_state TEXT NOT NULL DEFAULT 'idle',
            warm_cursor INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_active_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_partner_sessions_slug
            ON partner_sessions(slug);
        CREATE INDEX IF NOT EXISTS idx_partner_sessions_active
            ON partner_sessions(last_active_at);
        ",
    )?;

    Ok(())
}

/// Save a session to partner.db.
pub fn save_session(conn: &Connection, session: &Session) -> anyhow::Result<()> {
    let buffer_json = serde_json::to_string(&session.conversation_buffer)?;
    let topics_json = serde_json::to_string(&session.session_topics)?;
    let hydrated_json = serde_json::to_string(&session.hydrated_node_ids)?;
    let lifted_json = serde_json::to_string(&session.lifted_results)?;
    let state_str = serde_json::to_string(&session.dennis_state)?;

    conn.execute(
        "INSERT INTO partner_sessions
            (id, slug, is_lobby, conversation_buffer, session_topics,
             hydrated_node_ids, lifted_results, dennis_state, warm_cursor,
             created_at, last_active_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(id) DO UPDATE SET
            conversation_buffer = excluded.conversation_buffer,
            session_topics = excluded.session_topics,
            hydrated_node_ids = excluded.hydrated_node_ids,
            lifted_results = excluded.lifted_results,
            dennis_state = excluded.dennis_state,
            warm_cursor = excluded.warm_cursor,
            last_active_at = excluded.last_active_at",
        rusqlite::params![
            session.id,
            session.slug,
            session.is_lobby as i32,
            buffer_json,
            topics_json,
            hydrated_json,
            lifted_json,
            state_str,
            session.warm_cursor as i64,
            session.created_at,
            session.last_active_at,
        ],
    )?;

    Ok(())
}

/// Load a session from partner.db by ID.
pub fn load_session(conn: &Connection, session_id: &str) -> anyhow::Result<Option<Session>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, is_lobby, conversation_buffer, session_topics,
                hydrated_node_ids, lifted_results, dennis_state, warm_cursor,
                created_at, last_active_at
         FROM partner_sessions WHERE id = ?1",
    )?;

    let result = stmt.query_row(rusqlite::params![session_id], |row| {
        let id: String = row.get(0)?;
        let slug: Option<String> = row.get(1)?;
        let is_lobby: bool = row.get::<_, i32>(2)? != 0;
        let buffer_str: String = row.get(3)?;
        let topics_str: String = row.get(4)?;
        let hydrated_str: String = row.get(5)?;
        let lifted_str: String = row.get(6)?;
        let state_str: String = row.get(7)?;
        let warm_cursor: i64 = row.get(8)?;
        let created_at: String = row.get(9)?;
        let last_active_at: String = row.get(10)?;

        Ok((id, slug, is_lobby, buffer_str, topics_str, hydrated_str,
            lifted_str, state_str, warm_cursor, created_at, last_active_at))
    });

    match result {
        Ok((id, slug, is_lobby, buffer_str, topics_str, hydrated_str,
            lifted_str, state_str, warm_cursor, created_at, last_active_at)) => {
            let conversation_buffer: Vec<Message> =
                serde_json::from_str(&buffer_str).unwrap_or_default();
            let session_topics: Vec<SessionTopic> =
                serde_json::from_str(&topics_str).unwrap_or_default();
            let hydrated_node_ids: Vec<String> =
                serde_json::from_str(&hydrated_str).unwrap_or_default();
            let lifted_results: Vec<LiftedResult> =
                serde_json::from_str(&lifted_str).unwrap_or_default();
            let dennis_state: DennisState =
                serde_json::from_str(&state_str).unwrap_or_default();

            Ok(Some(Session {
                id,
                slug,
                is_lobby,
                conversation_buffer,
                session_topics,
                hydrated_node_ids,
                lifted_results,
                dennis_state,
                warm_cursor: warm_cursor as usize,
                created_at,
                last_active_at,
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// List all sessions, ordered by last_active_at descending.
pub fn list_sessions(conn: &Connection) -> anyhow::Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, is_lobby, dennis_state, warm_cursor, created_at, last_active_at
         FROM partner_sessions ORDER BY last_active_at DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let slug: Option<String> = row.get(1)?;
        let is_lobby: bool = row.get::<_, i32>(2)? != 0;
        let state_str: String = row.get(3)?;
        let _warm_cursor: i64 = row.get(4)?;
        let created_at: String = row.get(5)?;
        let last_active_at: String = row.get(6)?;

        Ok(SessionSummary {
            id,
            slug,
            is_lobby,
            dennis_state: serde_json::from_str(&state_str).unwrap_or_default(),
            created_at,
            last_active_at,
        })
    })?;

    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row?);
    }
    Ok(sessions)
}

/// Summary of a session (for listing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub slug: Option<String>,
    pub is_lobby: bool,
    pub dennis_state: DennisState,
    pub created_at: String,
    pub last_active_at: String,
}

// ── Constants ───────────────────────────────────────────────────────

/// Maximum conversation buffer size in estimated tokens.
pub const BUFFER_HARD_LIMIT: usize = 20_000;
/// Soft limit — triggers crystallization warning.
pub const BUFFER_SOFT_LIMIT: usize = 18_000;
/// Maximum tool calls per turn.
pub const MAX_TOOL_CALLS: usize = 5;
/// Nav skeleton token budget.
pub const NAV_SKELETON_BUDGET: usize = 5_000;
