// pyramid/preview.rs — WS-PREVIEW (Phase 3 of Episodic Memory Vine canonical v4)
//
// Preview-then-commit for new pyramid creation. Before committing to a build,
// operators see estimated cost, time, scope, and warnings. The preview is
// genuinely informative — not a loading spinner.
//
// See episodic-memory-vine-canonical-v4.md §8.2.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use std::path::Path;

use super::chain_loader;
use super::cost_model;
use super::ingest;
use super::types::{BuildPreview, ContentType, PreviewWarning};

// ── Constants ────────────────────────────────────────────────────────────────

/// Heuristic: average characters per token for estimating token counts from
/// file sizes. This is a rough approximation; the cost model uses observed
/// averages when available.
const CHARS_PER_TOKEN: f64 = 4.0;

/// Files above this size (in bytes) generate a warning.
const LARGE_FILE_THRESHOLD: u64 = 10_000_000; // 10 MB

/// Estimated seconds per chain step per file. Used when no observed timing
/// data is available. This is deliberately conservative (includes LLM
/// round-trip, parsing, DB writes).
const SECONDS_PER_STEP_PER_FILE: u64 = 12;

/// Estimated bytes of pyramid DB storage per source token. Accounts for
/// JSON node storage, indexes, and SQLite overhead.
const DISK_BYTES_PER_TOKEN: f64 = 2.5;

// ── Preview generation ───────────────────────────────────────────────────────

/// Generate a build preview by scanning the source directory, loading the chain
/// definition, and consulting the cost model. This gives the operator a genuine
/// picture of what they're committing to (§8.2).
///
/// Returns an error if the source path doesn't exist, the content type is
/// invalid, or the chain definition can't be loaded.
pub fn generate_build_preview(
    conn: &Connection,
    source_path: &str,
    content_type: &str,
    chain_id: &str,
    chains_dir: &Path,
) -> Result<BuildPreview> {
    // 1. Parse content type
    let ct = ContentType::from_str(content_type)
        .ok_or_else(|| anyhow::anyhow!("Invalid content_type: {content_type}"))?;

    // 2. Scan the source directory for files
    let files = ingest::scan_source_directory(source_path, &ct)
        .with_context(|| format!("Failed to scan source directory: {source_path}"))?;

    let file_count = files.len();

    // 3. Compute total bytes and estimated tokens
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();
    let estimated_total_tokens = (total_bytes as f64 / CHARS_PER_TOKEN) as usize;

    // 4. Generate warnings
    let mut warnings = Vec::new();
    for f in &files {
        if f.size == 0 {
            warnings.push(PreviewWarning {
                level: "warning".into(),
                file_path: Some(f.path.clone()),
                message: "Empty file — will be skipped during ingestion".into(),
            });
        } else if f.size > LARGE_FILE_THRESHOLD {
            warnings.push(PreviewWarning {
                level: "warning".into(),
                file_path: Some(f.path.clone()),
                message: format!(
                    "Large file ({:.1} MB) — may require extended processing time",
                    f.size as f64 / 1_000_000.0
                ),
            });
        }
    }

    if file_count == 0 {
        warnings.push(PreviewWarning {
            level: "error".into(),
            file_path: None,
            message: "No ingestible files found in the source directory".into(),
        });
    }

    // Check for unsupported file extensions by scanning for non-standard patterns
    // (only relevant for code/document types where we walk the directory)
    if matches!(ct, ContentType::Code | ContentType::Document) {
        let supported_exts: Vec<&str> = if ct == ContentType::Code {
            ingest::code_extensions().into_iter().collect()
        } else {
            ingest::doc_extensions().into_iter().collect()
        };
        // If the directory has many files but few matched, that's worth noting
        if let Ok(entries) = std::fs::read_dir(source_path) {
            let total_entries: usize = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .count();
            if total_entries > 0 && file_count < total_entries / 2 {
                warnings.push(PreviewWarning {
                    level: "info".into(),
                    file_path: None,
                    message: format!(
                        "{} of {} files in directory match supported extensions ({:?})",
                        file_count,
                        total_entries,
                        supported_exts,
                    ),
                });
            }
        }
    }

    // 5. Load chain definition to count steps
    let (step_count, chain_loaded) = load_chain_step_count(chain_id, chains_dir);
    if !chain_loaded {
        warnings.push(PreviewWarning {
            level: "warning".into(),
            file_path: None,
            message: format!(
                "Chain '{}' not found in chains directory — using default step estimate",
                chain_id
            ),
        });
    }

    // 6. Estimate pyramid structure
    //    Conversation: 1 pyramid per file, each with ~log2(chunks) layers
    //    Code/Document: 1 pyramid total, layers based on file count
    let (estimated_pyramids, estimated_layers, estimated_nodes) =
        estimate_pyramid_structure(&ct, file_count, estimated_total_tokens);

    // 7. Estimate cost using the cost model
    let avg_tokens_per_file = if file_count > 0 {
        estimated_total_tokens / file_count
    } else {
        0
    };
    let estimated_cost_dollars =
        estimate_build_cost(conn, chain_id, file_count, avg_tokens_per_file)?;

    // 8. Estimate wall-clock time
    let estimated_time_seconds = estimate_build_time(file_count, step_count);

    // 9. Estimate disk usage
    let estimated_disk_bytes = (estimated_total_tokens as f64 * DISK_BYTES_PER_TOKEN) as u64;

    Ok(BuildPreview {
        source_path: source_path.to_string(),
        content_type: content_type.to_string(),
        chain_id: chain_id.to_string(),
        file_count,
        estimated_total_tokens,
        estimated_pyramids,
        estimated_layers,
        estimated_nodes,
        estimated_cost_dollars,
        estimated_time_seconds,
        estimated_disk_bytes,
        warnings,
        generated_at: Utc::now().to_rfc3339(),
    })
}

// ── Cost estimation ─────────────────────────────────────────────────────────

/// Estimate the total USD cost for a build using the cost model.
///
/// Sums up `usd_per_conversation × file_count` for all cost model entries
/// whose `chain_phase` starts with the given `chain_id` prefix, or uses a
/// fallback heuristic based on token counts when no cost model data exists.
pub fn estimate_build_cost(
    conn: &Connection,
    chain_id: &str,
    file_count: usize,
    avg_tokens_per_file: usize,
) -> Result<f64> {
    // Try to get cost data from the cost model table
    let all_entries = cost_model::list_all(conn)?;

    // Sum up costs for phases matching this chain
    let mut total_cost = 0.0;
    let mut found_entries = false;

    for (_phase, entries) in &all_entries {
        for entry in entries {
            // Match phases that belong to this chain. Chain phases are typically
            // recorded as "chain_id.step_name" or just the step name for default chains.
            if entry.chain_phase.starts_with(chain_id)
                || entry.chain_phase.starts_with("extract")
                || entry.chain_phase.starts_with("compress")
                || entry.chain_phase.starts_with("synthesize")
                || entry.chain_phase.starts_with("fuse")
            {
                // Scale the per-conversation cost by file count
                total_cost += entry.usd_per_conversation * file_count as f64;
                found_entries = true;
            }
        }
    }

    if found_entries {
        return Ok(total_cost);
    }

    // Fallback heuristic when no cost model data exists:
    // Use default pricing from Tier1Config
    let tier1 = super::Tier1Config::default();
    let input_price = tier1.default_input_price_per_million;
    let output_price = tier1.default_output_price_per_million;

    // Estimate: each file needs ~3 LLM calls (extract, synthesize, fuse)
    // with avg_tokens_per_file input and ~25% output ratio
    let calls_per_file: f64 = 3.0;
    let output_ratio: f64 = 0.25;

    let cost_per_file = calls_per_file
        * cost_model::cost_per_call(
            avg_tokens_per_file as f64,
            avg_tokens_per_file as f64 * output_ratio,
            input_price,
            output_price,
        );

    Ok(cost_per_file * file_count as f64)
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Load a chain definition and return its step count. If the chain can't be
/// found or loaded, returns a default estimate of 5 steps.
fn load_chain_step_count(chain_id: &str, chains_dir: &Path) -> (usize, bool) {
    // Try to find the chain YAML by scanning defaults/ and variants/
    let candidates = [
        chains_dir.join("defaults").join(format!("{chain_id}.yaml")),
        chains_dir
            .join("defaults")
            .join(format!("{chain_id}.yml")),
        chains_dir.join("variants").join(format!("{chain_id}.yaml")),
        chains_dir
            .join("variants")
            .join(format!("{chain_id}.yml")),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            if let Ok(def) = chain_loader::load_chain(candidate, chains_dir) {
                return (count_steps_recursive(&def.steps), true);
            }
        }
    }

    // Try matching by chain id from discovered chains
    if let Ok(metas) = chain_loader::discover_chains(chains_dir) {
        for meta in &metas {
            if meta.id == chain_id {
                let path = Path::new(&meta.file_path);
                if let Ok(def) = chain_loader::load_chain(path, chains_dir) {
                    return (count_steps_recursive(&def.steps), true);
                }
                // Fall back to metadata step count
                return (meta.step_count, true);
            }
        }
    }

    // Default estimate when chain not found
    (5, false)
}

/// Count steps recursively (for_each steps contain nested steps).
fn count_steps_recursive(steps: &[super::chain_engine::ChainStep]) -> usize {
    let mut count = 0;
    for step in steps {
        count += 1;
        if let Some(ref inner) = step.steps {
            count += count_steps_recursive(inner);
        }
    }
    count
}

/// Estimate the pyramid structure (pyramids, layers, nodes) based on content
/// type, file count, and token volume.
fn estimate_pyramid_structure(
    ct: &ContentType,
    file_count: usize,
    total_tokens: usize,
) -> (usize, usize, usize) {
    match ct {
        ContentType::Conversation => {
            // Each conversation file becomes its own pyramid.
            // Each pyramid typically has ~3-5 layers depending on length.
            let pyramids = file_count;
            let avg_chunks_per_file = if file_count > 0 {
                let avg_tokens = total_tokens / file_count;
                // ~500 tokens per chunk (conversation chunk target)
                (avg_tokens / 500).max(1)
            } else {
                1
            };
            let layers = estimate_layer_count(avg_chunks_per_file);
            // Nodes: L0 = chunks, each layer above roughly halves
            let nodes_per_pyramid = estimate_node_count(avg_chunks_per_file, layers);
            (pyramids, layers, pyramids * nodes_per_pyramid)
        }
        ContentType::Code | ContentType::Document => {
            // Single pyramid for all files.
            let pyramids = 1;
            let layers = estimate_layer_count(file_count);
            let nodes = estimate_node_count(file_count, layers);
            (pyramids, layers, nodes)
        }
        ContentType::Vine | ContentType::Question => {
            // Vine/question types don't have file-based scanning
            (0, 0, 0)
        }
    }
}

/// Estimate the number of layers given the L0 node count. Each layer roughly
/// groups 3-5 nodes from below into 1 node above. log_base4(n) gives a
/// reasonable estimate.
fn estimate_layer_count(l0_count: usize) -> usize {
    if l0_count <= 1 {
        return 1;
    }
    // log4(n) + 1 for the base layer
    let layers = (l0_count as f64).log(4.0).ceil() as usize + 1;
    layers.max(2) // minimum 2 layers (L0 + apex)
}

/// Estimate total node count across all layers given L0 count and layer count.
/// Each layer has roughly 1/4 the nodes of the layer below.
fn estimate_node_count(l0_count: usize, layers: usize) -> usize {
    let mut total = 0;
    let mut current_layer_count = l0_count;
    for _ in 0..layers {
        total += current_layer_count;
        current_layer_count = (current_layer_count / 4).max(1);
        if current_layer_count <= 1 {
            total += 1; // apex
            break;
        }
    }
    total.max(1)
}

/// Estimate wall-clock build time in seconds.
fn estimate_build_time(file_count: usize, step_count: usize) -> u64 {
    if file_count == 0 {
        return 0;
    }
    // Each file goes through each step. Steps have some parallelism but
    // LLM calls dominate. Conservative estimate.
    let raw = (file_count as u64) * (step_count as u64) * SECONDS_PER_STEP_PER_FILE;
    // Apply concurrency discount (typical concurrency of 5)
    let concurrent = raw / 5;
    concurrent.max(10) // minimum 10 seconds for any non-empty build
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;

    /// Set up an in-memory DB with the cost model table.
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pyramid_chain_cost_model (
                chain_phase TEXT NOT NULL,
                model TEXT NOT NULL,
                avg_input_tokens REAL NOT NULL,
                avg_output_tokens REAL NOT NULL,
                calls_per_conversation REAL NOT NULL,
                usd_per_call REAL NOT NULL,
                usd_per_conversation REAL NOT NULL,
                is_heuristic INTEGER NOT NULL DEFAULT 1,
                sample_count INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (chain_phase, model)
            );
            CREATE TABLE IF NOT EXISTS pyramid_llm_audit (
                id INTEGER PRIMARY KEY,
                build_id TEXT,
                step_name TEXT,
                model TEXT,
                prompt_tokens INTEGER,
                completion_tokens INTEGER,
                status TEXT,
                created_at TEXT
            );",
        )
        .unwrap();
        conn
    }

    /// Create a temp directory with some .jsonl files for testing.
    fn create_test_source_dir(file_count: usize, empty_file: bool) -> TempDir {
        let dir = TempDir::new().unwrap();
        for i in 0..file_count {
            let path = dir.path().join(format!("conversation_{}.jsonl", i));
            if empty_file && i == 0 {
                fs::write(&path, "").unwrap();
            } else {
                // Write ~1000 chars of content per file
                let content = format!(
                    "{{\"role\": \"user\", \"content\": \"{}\"}}\n",
                    "test content ".repeat(70)
                );
                fs::write(&path, &content).unwrap();
            }
        }
        dir
    }

    #[test]
    fn test_preview_three_jsonl_files() {
        let conn = setup_test_db();
        let dir = create_test_source_dir(3, false);
        let chains_dir = TempDir::new().unwrap();
        // Create minimal chains directory structure
        fs::create_dir_all(chains_dir.path().join("defaults")).unwrap();

        let preview = generate_build_preview(
            &conn,
            dir.path().to_str().unwrap(),
            "conversation",
            "conversation-default",
            chains_dir.path(),
        )
        .unwrap();

        assert_eq!(preview.file_count, 3);
        assert_eq!(preview.content_type, "conversation");
        assert_eq!(preview.chain_id, "conversation-default");
        assert!(preview.estimated_total_tokens > 0);
        assert_eq!(preview.estimated_pyramids, 3); // 1 per file for conversation
        assert!(preview.estimated_layers >= 1);
        assert!(preview.estimated_nodes >= 3);
        assert!(preview.estimated_cost_dollars > 0.0);
        assert!(preview.estimated_time_seconds > 0);
        assert!(preview.estimated_disk_bytes > 0);
        // The only warning should be about the chain not being found
        // (test uses an empty chains dir). No file-level warnings.
        let file_warnings: Vec<_> = preview
            .warnings
            .iter()
            .filter(|w| w.file_path.is_some())
            .collect();
        assert!(
            file_warnings.is_empty(),
            "Expected no file-level warnings, got: {:?}",
            file_warnings
        );
    }

    #[test]
    fn test_preview_warns_on_empty_files() {
        let conn = setup_test_db();
        let dir = create_test_source_dir(3, true); // first file is empty
        let chains_dir = TempDir::new().unwrap();
        fs::create_dir_all(chains_dir.path().join("defaults")).unwrap();

        let preview = generate_build_preview(
            &conn,
            dir.path().to_str().unwrap(),
            "conversation",
            "conversation-default",
            chains_dir.path(),
        )
        .unwrap();

        assert_eq!(preview.file_count, 3);
        // Should have at least one warning about the empty file
        let empty_warnings: Vec<_> = preview
            .warnings
            .iter()
            .filter(|w| w.message.contains("Empty file"))
            .collect();
        assert_eq!(
            empty_warnings.len(),
            1,
            "Expected exactly 1 empty file warning, got: {:?}",
            preview.warnings
        );
        assert_eq!(empty_warnings[0].level, "warning");
        assert!(empty_warnings[0].file_path.is_some());
    }

    #[test]
    fn test_cost_estimation_uses_seed_data() {
        let conn = setup_test_db();

        // Seed the cost model with known data
        conn.execute(
            "INSERT INTO pyramid_chain_cost_model
                (chain_phase, model, avg_input_tokens, avg_output_tokens,
                 calls_per_conversation, usd_per_call, usd_per_conversation,
                 is_heuristic, sample_count, updated_at)
             VALUES ('extract', 'test-model', 8000.0, 1500.0, 20.0, 0.01, 0.20, 1, 0, 0)",
            [],
        )
        .unwrap();

        let cost = estimate_build_cost(&conn, "conversation-default", 5, 2000).unwrap();

        // Should use the seeded data: 0.20 per conversation × 5 files = 1.00
        assert!(
            cost > 0.0,
            "Cost should be positive when seed data exists"
        );
        assert!(
            (cost - 1.0).abs() < 0.01,
            "Expected cost ~1.00, got {}",
            cost
        );
    }

    #[test]
    fn test_commit_creates_dadbear_config() {
        // This test verifies that the commit pathway creates a DADBEAR watch config.
        // We test the DB layer directly since the HTTP handler is async.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pyramid_dadbear_config (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slug TEXT NOT NULL,
                source_path TEXT NOT NULL,
                content_type TEXT NOT NULL,
                scan_interval_secs INTEGER NOT NULL DEFAULT 10,
                debounce_secs INTEGER NOT NULL DEFAULT 30,
                session_timeout_secs INTEGER NOT NULL DEFAULT 1800,
                batch_size INTEGER NOT NULL DEFAULT 1,
                enabled INTEGER NOT NULL DEFAULT 1,
                last_scan_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(slug, source_path)
            );",
        )
        .unwrap();

        let config = super::super::types::DadbearWatchConfig {
            id: 0,
            slug: "test-pyramid".into(),
            source_path: "/tmp/test-source".into(),
            content_type: "conversation".into(),
            scan_interval_secs: 10,
            debounce_secs: 30,
            session_timeout_secs: 1800,
            batch_size: 1,
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        };

        let id = super::super::db::save_dadbear_config(&conn, &config).unwrap();
        assert!(id > 0);

        // Verify it was saved
        let configs = super::super::db::get_dadbear_configs(&conn, "test-pyramid").unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].slug, "test-pyramid");
        assert_eq!(configs[0].source_path, "/tmp/test-source");
        assert_eq!(configs[0].content_type, "conversation");
        assert!(configs[0].enabled);
    }
}
