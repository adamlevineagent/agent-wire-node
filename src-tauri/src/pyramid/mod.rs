// pyramid/ — Knowledge Pyramid engine
//
// Modules:
//   db      — SQLite schema, migrations, CRUD operations
//   types   — Data model structs (PyramidNode, Slug, Topic, etc.)
//   ingest  — Content ingestion (conversation, code, document)
//   build   — LLM-powered build pipeline (3 variants)
//   query   — Query functions (apex, search, drill, entities, resolved)
//   llm     — OpenRouter API client with 3-tier model cascade
//   slug    — Slug/namespace management
//   routes  — Warp HTTP route handlers

pub mod db;
pub mod types;
pub mod ingest;
pub mod build;
pub mod query;
pub mod llm;
pub mod slug;
pub mod routes;

use std::sync::Arc;
use tokio::sync::Mutex;
use rusqlite::Connection;

use self::llm::LlmConfig;
use self::types::BuildStatus;

/// Shared state for the pyramid engine.
///
/// Two SQLite connections: `reader` for concurrent reads, `writer` for
/// serialized writes. Both point to the same WAL-mode database file.
pub struct PyramidState {
    /// Read-only connection for query operations.
    pub reader: Arc<Mutex<Connection>>,
    /// Write connection for mutations (slug creation, node saves, etc.)
    pub writer: Arc<Mutex<Connection>>,
    /// LLM configuration (API key, model cascade).
    pub config: Arc<tokio::sync::RwLock<LlmConfig>>,
    /// Currently active build, if any.
    pub active_build: Arc<tokio::sync::RwLock<Option<BuildHandle>>>,
}

/// Handle to a running pyramid build.
pub struct BuildHandle {
    /// Slug being built.
    pub slug: String,
    /// Cancellation token — cancel to abort the build.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Live status (progress, elapsed time, etc.)
    pub status: Arc<tokio::sync::RwLock<BuildStatus>>,
}
