// pyramid/chain_resolver.rs — Two-tier chain resolution (local + Wire)
//
// Extends the existing chain_loader with a two-tier lookup:
// 1. Check local templates (existing behavior via chain_loader)
// 2. If not found locally, check Wire via WireImportClient
// 3. Cache imported chains locally in SQLite
//
// Phase 4.2: Chain import + compiler decoupling.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::chain_engine::ChainDefinition;
use super::chain_loader;
use super::wire_import::{self, WireImportClient};

// ─── Types ───────────────────────────────────────────────────

/// Where a resolved chain came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChainSource {
    /// Loaded from local YAML files (defaults/ or variants/)
    Local { file_path: String },
    /// Imported from the Wire marketplace
    Wire { contribution_id: String },
    /// Loaded from SQLite cache of a previously imported Wire chain
    CachedWire { contribution_id: String },
}

/// A resolved chain with provenance tracking.
#[derive(Debug, Clone)]
pub struct ResolvedChain {
    /// The chain definition (deserialized from YAML or Wire JSON)
    pub definition: ChainDefinition,
    /// Where this chain came from
    pub source: ChainSource,
}

/// A resolved question set with provenance.
#[derive(Debug, Clone)]
pub struct ResolvedQuestionSet {
    /// The question set definition (raw JSON)
    pub definition: serde_json::Value,
    /// Wire contribution ID
    pub contribution_id: String,
    /// Where this came from
    pub source: ChainSource,
}

/// Configuration for the chain resolver.
#[derive(Debug, Clone)]
pub struct ChainResolverConfig {
    /// Path to the local chains directory
    pub chains_dir: PathBuf,
    /// Whether Wire lookup is enabled
    pub wire_enabled: bool,
    /// Wire API base URL
    pub wire_url: Option<String>,
    /// Wire auth token
    pub wire_auth_token: Option<String>,
}

// ─── Resolver ────────────────────────────────────────────────

/// Two-tier chain resolver: local templates first, then Wire.
pub struct ChainResolver {
    config: ChainResolverConfig,
    /// Wire import client (lazily initialized when Wire is enabled)
    wire_client: Option<WireImportClient>,
    /// SQLite connection for persisting imported chains
    db: Arc<Mutex<Connection>>,
}

impl ChainResolver {
    /// Create a new chain resolver.
    pub fn new(config: ChainResolverConfig, db: Arc<Mutex<Connection>>) -> Self {
        let wire_client = if config.wire_enabled {
            match (&config.wire_url, &config.wire_auth_token) {
                (Some(url), Some(token)) if !url.is_empty() && !token.is_empty() => {
                    Some(WireImportClient::new(url.clone(), token.clone(), None))
                }
                _ => {
                    tracing::warn!("wire import enabled but missing wire_url or wire_auth_token");
                    None
                }
            }
        } else {
            None
        };

        Self {
            config,
            wire_client,
            db,
        }
    }

    /// Create a resolver with a pre-built Wire client (for testing).
    pub fn with_wire_client(
        config: ChainResolverConfig,
        db: Arc<Mutex<Connection>>,
        wire_client: WireImportClient,
    ) -> Self {
        Self {
            config,
            wire_client: Some(wire_client),
            db,
        }
    }

    /// Resolve a chain by ID.
    ///
    /// Two-tier lookup:
    /// 1. Try local templates (defaults/ and variants/ directories)
    /// 2. If not found and Wire is enabled, try Wire import
    /// 3. On Wire hit, persist to SQLite for offline access
    pub async fn resolve_chain(&self, chain_id: &str) -> Result<ResolvedChain> {
        // Tier 1: Local lookup
        match self.resolve_local(chain_id) {
            Ok(resolved) => {
                tracing::debug!(chain_id, "resolved chain from local templates");
                return Ok(resolved);
            }
            Err(_) => {
                tracing::debug!(chain_id, "chain not found locally, checking Wire");
            }
        }

        // Tier 1.5: Check SQLite cache for previously imported chains
        match self.resolve_cached_import(chain_id).await {
            Ok(Some(resolved)) => {
                tracing::debug!(chain_id, "resolved chain from SQLite import cache");
                return Ok(resolved);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(chain_id, error = %e, "failed to check import cache");
            }
        }

        // Tier 2: Wire lookup
        if let Some(ref client) = self.wire_client {
            match self.resolve_from_wire(client, chain_id).await {
                Ok(resolved) => {
                    tracing::info!(chain_id, "resolved chain from Wire");
                    return Ok(resolved);
                }
                Err(e) => {
                    tracing::warn!(chain_id, error = %e, "Wire chain lookup failed");
                    return Err(e).context(format!(
                        "chain '{}' not found locally and Wire lookup failed",
                        chain_id
                    ));
                }
            }
        }

        anyhow::bail!(
            "chain '{}' not found in local templates{}",
            chain_id,
            if self.wire_client.is_some() {
                " or Wire"
            } else {
                " (Wire import disabled)"
            }
        )
    }

    /// Resolve a question set from the Wire, with SQLite cache fallback.
    pub async fn resolve_question_set(&self, contribution_id: &str) -> Result<ResolvedQuestionSet> {
        // Check SQLite cache first for offline access
        {
            let conn = self.db.lock().await;
            match wire_import::load_imported_question_set(&conn, contribution_id) {
                Ok(Some(qs)) => {
                    tracing::debug!(
                        id = contribution_id,
                        "resolved question set from SQLite import cache"
                    );
                    return Ok(ResolvedQuestionSet {
                        definition: qs.question_set_definition,
                        contribution_id: qs.id,
                        source: ChainSource::CachedWire {
                            contribution_id: contribution_id.to_string(),
                        },
                    });
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        id = contribution_id,
                        error = %e,
                        "failed to check question set import cache"
                    );
                }
            }
        }

        let client = self.wire_client.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Wire import is not enabled — cannot fetch question sets")
        })?;

        let qs = client
            .fetch_question_set(contribution_id)
            .await
            .context("failed to fetch question set from Wire")?;

        // Persist to SQLite
        {
            let conn = self.db.lock().await;
            if let Err(e) = wire_import::save_imported_question_set(&conn, &qs) {
                tracing::warn!(
                    id = contribution_id,
                    error = %e,
                    "failed to persist imported question set to SQLite"
                );
            }
        }

        Ok(ResolvedQuestionSet {
            definition: qs.question_set_definition,
            contribution_id: qs.id,
            source: ChainSource::Wire {
                contribution_id: contribution_id.to_string(),
            },
        })
    }

    // ── Internal methods ─────────────────────────────────────

    /// Try to resolve a chain from local YAML files.
    fn resolve_local(&self, chain_id: &str) -> Result<ResolvedChain> {
        let chains_dir = &self.config.chains_dir;

        // Check defaults/ and variants/ directories
        let search_dirs = [chains_dir.join("defaults"), chains_dir.join("variants")];

        for dir in &search_dirs {
            if !dir.exists() {
                continue;
            }

            // Try both .yaml and .yml extensions
            for ext in &["yaml", "yml"] {
                // Try matching by filename (chain_id.yaml)
                let path = dir.join(format!("{}.{}", chain_id, ext));
                if path.exists() {
                    return self.load_local_chain(&path, chains_dir);
                }
            }

            // Scan directory for matching chain ID inside the YAML
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .extension()
                        .map(|e| e == "yaml" || e == "yml")
                        .unwrap_or(false)
                    {
                        if let Ok(def) = chain_loader::load_chain(&path, chains_dir) {
                            if def.id == chain_id {
                                return Ok(ResolvedChain {
                                    source: ChainSource::Local {
                                        file_path: path.to_string_lossy().into_owned(),
                                    },
                                    definition: def,
                                });
                            }
                        }
                    }
                }
            }
        }

        anyhow::bail!("chain '{}' not found in local templates", chain_id)
    }

    fn load_local_chain(&self, path: &Path, chains_dir: &Path) -> Result<ResolvedChain> {
        let def = chain_loader::load_chain(path, chains_dir)?;
        Ok(ResolvedChain {
            source: ChainSource::Local {
                file_path: path.to_string_lossy().into_owned(),
            },
            definition: def,
        })
    }

    /// Check SQLite for a previously imported chain.
    async fn resolve_cached_import(&self, chain_id: &str) -> Result<Option<ResolvedChain>> {
        let conn = self.db.lock().await;

        let imported = wire_import::load_imported_chain(&conn, chain_id)?;
        match imported {
            Some(chain) => {
                // Try to parse the Wire definition into a ChainDefinition
                match serde_json::from_value::<ChainDefinition>(chain.definition.clone()) {
                    Ok(def) => Ok(Some(ResolvedChain {
                        source: ChainSource::CachedWire {
                            contribution_id: chain.id,
                        },
                        definition: def,
                    })),
                    Err(e) => {
                        tracing::warn!(
                            id = chain_id,
                            error = %e,
                            "cached import exists but failed to parse as ChainDefinition"
                        );
                        Ok(None)
                    }
                }
            }
            None => Ok(None),
        }
    }

    /// Fetch a chain from the Wire and persist it locally.
    async fn resolve_from_wire(
        &self,
        client: &WireImportClient,
        chain_id: &str,
    ) -> Result<ResolvedChain> {
        let imported = client
            .fetch_chain(chain_id)
            .await
            .context("failed to fetch chain from Wire")?;

        // Persist to SQLite for offline access
        {
            let conn = self.db.lock().await;
            if let Err(e) = wire_import::save_imported_chain(&conn, &imported) {
                tracing::warn!(
                    id = chain_id,
                    error = %e,
                    "failed to persist imported chain to SQLite"
                );
            }
        }

        // Try to parse the Wire definition into a ChainDefinition
        let def = serde_json::from_value::<ChainDefinition>(imported.definition.clone())
            .context("Wire chain definition is not a valid ChainDefinition")?;

        Ok(ResolvedChain {
            source: ChainSource::Wire {
                contribution_id: imported.id,
            },
            definition: def,
        })
    }
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_test_chain_yaml(id: &str) -> String {
        format!(
            r#"schema_version: 1
id: "{}"
name: "Test Chain {}"
description: "A test chain"
content_type: "code"
version: "0.1.0"
author: "test"
defaults:
  model_tier: "mid"
  temperature: 0.2
  on_error: "retry(2)"
steps:
  - name: "placeholder"
    primitive: "extract"
    instruction: "Test instruction"
"#,
            id, id
        )
    }

    #[test]
    fn test_resolve_local_by_filename() {
        let tmp = TempDir::new().unwrap();
        let chains_dir = tmp.path();

        // Create defaults directory with a chain
        let defaults_dir = chains_dir.join("defaults");
        fs::create_dir_all(&defaults_dir).unwrap();
        fs::write(
            defaults_dir.join("code-default.yaml"),
            make_test_chain_yaml("code-default"),
        )
        .unwrap();

        let config = ChainResolverConfig {
            chains_dir: chains_dir.to_path_buf(),
            wire_enabled: false,
            wire_url: None,
            wire_auth_token: None,
        };

        let conn = Connection::open_in_memory().unwrap();
        wire_import::init_import_tables(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let resolver = ChainResolver::new(config, db);
        let result = resolver.resolve_local("code-default");
        assert!(result.is_ok());

        let resolved = result.unwrap();
        assert_eq!(resolved.definition.id, "code-default");
        match resolved.source {
            ChainSource::Local { ref file_path } => {
                assert!(file_path.contains("code-default.yaml"));
            }
            _ => panic!("expected local source"),
        }
    }

    #[test]
    fn test_resolve_local_by_id_scan() {
        let tmp = TempDir::new().unwrap();
        let chains_dir = tmp.path();

        // Create defaults directory with a chain that has a different filename
        let defaults_dir = chains_dir.join("defaults");
        fs::create_dir_all(&defaults_dir).unwrap();
        fs::write(
            defaults_dir.join("my-custom-chain.yaml"),
            make_test_chain_yaml("custom-scan-test"),
        )
        .unwrap();

        let config = ChainResolverConfig {
            chains_dir: chains_dir.to_path_buf(),
            wire_enabled: false,
            wire_url: None,
            wire_auth_token: None,
        };

        let conn = Connection::open_in_memory().unwrap();
        wire_import::init_import_tables(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let resolver = ChainResolver::new(config, db);
        let result = resolver.resolve_local("custom-scan-test");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().definition.id, "custom-scan-test");
    }

    #[test]
    fn test_resolve_local_not_found() {
        let tmp = TempDir::new().unwrap();
        let chains_dir = tmp.path();
        fs::create_dir_all(chains_dir.join("defaults")).unwrap();

        let config = ChainResolverConfig {
            chains_dir: chains_dir.to_path_buf(),
            wire_enabled: false,
            wire_url: None,
            wire_auth_token: None,
        };

        let conn = Connection::open_in_memory().unwrap();
        wire_import::init_import_tables(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let resolver = ChainResolver::new(config, db);
        let result = resolver.resolve_local("nonexistent-chain");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resolve_cached_import_hit() {
        let conn = Connection::open_in_memory().unwrap();
        wire_import::init_import_tables(&conn).unwrap();

        // Pre-populate SQLite with an imported chain
        let chain_def = serde_json::json!({
            "schema_version": 1,
            "id": "wire-chain-001",
            "name": "Wire Chain",
            "description": "From the Wire",
            "content_type": "code",
            "version": "1.0.0",
            "author": "wire-agent",
            "defaults": {"model_tier": "mid", "temperature": 0.2, "on_error": "retry(2)"},
            "steps": [{"name": "extract", "primitive": "extract", "instruction": "Do the thing"}]
        });

        let imported = wire_import::ImportedChain {
            id: "wire-chain-001".into(),
            title: "Wire Chain".into(),
            teaser: "From the Wire".into(),
            content_type: Some("code".into()),
            definition: chain_def,
            topics: vec![],
            structured_data: None,
            fetched_at: "2026-03-25T00:00:00Z".into(),
            contribution_type: Some("action".into()),
        };
        wire_import::save_imported_chain(&conn, &imported).unwrap();

        let db = Arc::new(Mutex::new(conn));

        let tmp = TempDir::new().unwrap();
        let config = ChainResolverConfig {
            chains_dir: tmp.path().to_path_buf(),
            wire_enabled: false,
            wire_url: None,
            wire_auth_token: None,
        };

        let resolver = ChainResolver::new(config, db);
        let result = resolver.resolve_cached_import("wire-chain-001").await;
        assert!(result.is_ok());

        let resolved = result.unwrap();
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.definition.id, "wire-chain-001");
        assert!(matches!(resolved.source, ChainSource::CachedWire { .. }));
    }

    #[tokio::test]
    async fn test_resolve_cached_import_miss() {
        let conn = Connection::open_in_memory().unwrap();
        wire_import::init_import_tables(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let tmp = TempDir::new().unwrap();
        let config = ChainResolverConfig {
            chains_dir: tmp.path().to_path_buf(),
            wire_enabled: false,
            wire_url: None,
            wire_auth_token: None,
        };

        let resolver = ChainResolver::new(config, db);
        let result = resolver.resolve_cached_import("nonexistent").await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_full_resolve_local_first() {
        let tmp = TempDir::new().unwrap();
        let chains_dir = tmp.path();
        let defaults_dir = chains_dir.join("defaults");
        fs::create_dir_all(&defaults_dir).unwrap();
        fs::write(
            defaults_dir.join("code-default.yaml"),
            make_test_chain_yaml("code-default"),
        )
        .unwrap();

        let conn = Connection::open_in_memory().unwrap();
        wire_import::init_import_tables(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let config = ChainResolverConfig {
            chains_dir: chains_dir.to_path_buf(),
            wire_enabled: false,
            wire_url: None,
            wire_auth_token: None,
        };

        let resolver = ChainResolver::new(config, db);
        let result = resolver.resolve_chain("code-default").await;
        assert!(result.is_ok());

        let resolved = result.unwrap();
        assert!(matches!(resolved.source, ChainSource::Local { .. }));
    }

    #[tokio::test]
    async fn test_full_resolve_not_found_no_wire() {
        let tmp = TempDir::new().unwrap();
        let chains_dir = tmp.path();
        fs::create_dir_all(chains_dir.join("defaults")).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        wire_import::init_import_tables(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let config = ChainResolverConfig {
            chains_dir: chains_dir.to_path_buf(),
            wire_enabled: false,
            wire_url: None,
            wire_auth_token: None,
        };

        let resolver = ChainResolver::new(config, db);
        let result = resolver.resolve_chain("nonexistent").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("not found"));
        assert!(err_msg.contains("Wire import disabled"));
    }
}
