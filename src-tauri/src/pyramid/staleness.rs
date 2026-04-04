// pyramid/staleness.rs — Weight-Based Staleness Detection & Propagation (Channel A)
//
// When source files change, this module:
//   1. detect_source_changes()  — records file-level deltas
//   2. propagate_staleness()    — traces evidence weights upward through layers,
//                                  attenuating as it climbs (L0→L1 weight × L1→L2 weight)
//   3. process_staleness_queue() — dequeues items for the build runner to re-answer
//
// Key invariant: staleness ATTENUATES through layers. Distant changes have less
// impact than direct citations. The threshold (default 0.3) determines which
// questions get enqueued for re-answering.

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use super::db;
use super::types::{EvidenceVerdict, SourceDelta, StalenessItem};

// ── Public Types ──────────────────────────────────────────────────────────────

/// A file that changed on disk, triggering staleness detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub change_type: ChangeType,
}

/// How a source file changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Addition,
    Modification,
    Supersession,
    Deletion,
}

impl ChangeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeType::Addition => "addition",
            ChangeType::Modification => "modification",
            ChangeType::Supersession => "supersession",
            ChangeType::Deletion => "deletion",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "addition" => ChangeType::Addition,
            "modification" => ChangeType::Modification,
            "supersession" => ChangeType::Supersession,
            "deletion" => ChangeType::Deletion,
            other => {
                warn!("Unknown change type: '{other}', defaulting to Modification");
                ChangeType::Modification
            }
        }
    }

    /// Whether this change type should skip attenuation on the first hop
    /// (i.e., the initial evidence link carries the full incoming score).
    pub fn skip_first_attenuation(&self) -> bool {
        matches!(self, ChangeType::Supersession | ChangeType::Deletion)
    }
}

/// Report from propagate_staleness: what questions were affected and how much.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StalenessReport {
    /// Question IDs that exceeded the staleness threshold.
    pub affected_questions: Vec<String>,
    /// Deepest layer reached during propagation.
    pub max_depth_reached: i64,
    /// Every question encountered during propagation mapped to its staleness score.
    pub staleness_scores: HashMap<String, f64>,
}

// ── Default Threshold ─────────────────────────────────────────────────────────

/// 11-F: Deprecated — use OperationalConfig.tier2.staleness_threshold (default: 0.3) instead.
/// Retained only for backward compatibility. All production code reads from config.
#[deprecated(note = "Use OperationalConfig.tier2.staleness_threshold instead")]
pub const DEFAULT_STALENESS_THRESHOLD: f64 = 0.3;

// ── Step 1: Detect Source Changes ─────────────────────────────────────────────

/// Records which source files changed. Saves each delta to `pyramid_source_deltas`
/// and returns the created `SourceDelta` records.
///
/// The caller (file watcher or manual trigger) provides the changed files.
/// Each gets persisted so we have an audit trail even if propagation crashes.
pub fn detect_source_changes(
    conn: &Connection,
    slug: &str,
    changed_files: &[ChangedFile],
) -> Result<Vec<SourceDelta>> {
    for cf in changed_files {
        db::save_source_delta(conn, slug, &cf.path, cf.change_type.as_str(), None)?;
        info!(slug, path = %cf.path, change = cf.change_type.as_str(), "Source delta saved");
    }

    // Read back the unprocessed deltas (includes the ones we just saved plus
    // any previously unprocessed ones — the caller gets the full pending set).
    let deltas = db::get_unprocessed_source_deltas(conn, slug)?;

    Ok(deltas)
}

// ── Step 2: Propagate Staleness ───────────────────────────────────────────────

/// Traces evidence weights upward from changed L0 nodes through the pyramid.
///
/// Algorithm:
///   a. For each delta's file_path → look up L0 node IDs from pyramid_file_hashes
///   b. For each L0 node → follow evidence links upward:
///      - Each KEEP link carries a weight (0.0-1.0)
///      - Staleness score = product of weights along the path (attenuation)
///      - For multi-hop: L0→L1 weight × L1→L2 weight × ...
///   c. Questions above `threshold` get enqueued to pyramid_staleness_queue
///
/// Marks each source delta as processed after propagation.
pub fn propagate_staleness(
    conn: &Connection,
    slug: &str,
    deltas: &[SourceDelta],
    threshold: f64,
) -> Result<StalenessReport> {
    let mut all_scores: HashMap<String, f64> = HashMap::new();
    let mut max_depth: i64 = 0;

    // Collect all affected L0 node IDs from file hashes, tracking the highest-priority
    // change type per node (Deletion/Supersession skip first-hop attenuation).
    let mut l0_change_types: HashMap<String, ChangeType> = HashMap::new();
    for delta in deltas {
        let node_ids = get_l0_node_ids_for_file(conn, slug, &delta.file_path)?;
        if node_ids.is_empty() {
            debug!(slug, path = %delta.file_path, "No L0 nodes found for changed file, skipping");
        }
        let ct = ChangeType::from_str(&delta.change_type);
        for nid in node_ids {
            let existing = l0_change_types.get(&nid);
            // Prefer a change type that skips first attenuation over one that doesn't
            let dominated = match existing {
                Some(prev) if prev.skip_first_attenuation() => true,
                _ => false,
            };
            if !dominated {
                l0_change_types.insert(nid, ct.clone());
            }
        }
    }

    info!(
        slug,
        l0_count = l0_change_types.len(),
        "Propagating staleness from L0 nodes"
    );

    // best_scores tracks the highest score propagated through each node across ALL
    // L0 starting points. A node is only re-propagated when a higher score arrives.
    let mut best_scores: HashMap<String, f64> = HashMap::new();

    // For each L0 node, walk evidence links upward accumulating attenuated scores
    for (l0_id, change_type) in &l0_change_types {
        // Start with score 1.0 at the L0 node itself (it changed, so it's fully stale)
        let skip_first_attenuation = change_type.skip_first_attenuation();
        propagate_from_node(
            conn,
            slug,
            l0_id,
            1.0,
            0,
            &mut all_scores,
            &mut max_depth,
            &mut best_scores,
            skip_first_attenuation,
        )?;
    }

    // Enqueue questions above threshold
    let mut affected_questions: Vec<String> = Vec::new();
    for (question_id, &score) in &all_scores {
        if score >= threshold {
            let reason = format!(
                "Weight-based staleness: score {:.3} >= threshold {:.3}",
                score, threshold
            );
            db::enqueue_staleness(conn, slug, question_id, &reason, "channel_a", score)?;
            affected_questions.push(question_id.clone());
            info!(slug, question_id, score, "Enqueued stale question");
        } else {
            debug!(
                slug,
                question_id, score, threshold, "Below threshold, not enqueued"
            );
        }
    }

    // Mark all source deltas as processed
    for delta in deltas {
        db::mark_source_delta_processed(conn, delta.id)?;
    }

    affected_questions.sort();

    info!(
        slug,
        affected = affected_questions.len(),
        total_scored = all_scores.len(),
        max_depth = max_depth,
        "Staleness propagation complete"
    );

    Ok(StalenessReport {
        affected_questions,
        max_depth_reached: max_depth,
        staleness_scores: all_scores,
    })
}

/// Recursively propagate staleness score upward through evidence links.
///
/// At each level, the incoming `score` is multiplied by the evidence weight
/// to the next layer (attenuation). The maximum score seen for each question
/// is kept (if the same question is reached via multiple paths, the highest
/// score wins).
///
/// `best_scores` replaces the old `visited` HashSet — a node is only
/// re-propagated when a strictly higher score arrives, preserving cycle
/// detection (same or lower score = skip) while allowing higher-scoring
/// paths through shared nodes.
///
/// `skip_attenuation` causes the first hop from this node to pass the
/// incoming score through without multiplying by edge weight. This is used
/// for Deletion and Supersession change types so the initial evidence link
/// carries full staleness. After the first hop, attenuation resumes normally.
fn propagate_from_node(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    incoming_score: f64,
    current_depth: i64,
    scores: &mut HashMap<String, f64>,
    max_depth: &mut i64,
    best_scores: &mut HashMap<String, f64>,
    skip_attenuation: bool,
) -> Result<()> {
    // Score-based cycle/redundancy detection: skip if we already propagated
    // from this node with an equal or higher score.
    let prev_best = best_scores.get(node_id).copied().unwrap_or(-1.0);
    if incoming_score <= prev_best {
        return Ok(());
    }
    best_scores.insert(node_id.to_string(), incoming_score);

    // Update max depth
    if current_depth > *max_depth {
        *max_depth = current_depth;
    }

    // Safety: stop propagation if we've gone too deep
    let max_propagation_depth = super::Tier3Config::default().staleness_max_propagation_depth;
    if current_depth >= max_propagation_depth {
        warn!(
            slug,
            node_id, current_depth, "Hit max propagation depth, stopping"
        );
        return Ok(());
    }

    // Find all evidence links where this node is the source
    let evidence_links = db::get_evidence_for_source_cross(conn, node_id)?;

    for link in &evidence_links {
        // Only follow KEEP links — DISCONNECT/MISSING don't propagate staleness
        if link.verdict != EvidenceVerdict::Keep {
            continue;
        }

        let link_weight = link.weight.unwrap_or(1.0);
        let attenuated_score = if skip_attenuation {
            // Deletion/Supersession: first hop passes full score through
            incoming_score
        } else {
            incoming_score * link_weight
        };

        if attenuated_score < 0.001 {
            continue;
        }

        let target_id = &link.target_node_id;

        // Keep the maximum score if we reach the same question via multiple paths
        let current = scores.get(target_id).copied().unwrap_or(0.0);
        if attenuated_score > current {
            scores.insert(target_id.clone(), attenuated_score);
        }

        // Continue propagating upward from the target node
        // After the first hop, attenuation always resumes (skip_attenuation = false)
        propagate_from_node(
            conn,
            slug,
            target_id,
            attenuated_score,
            current_depth + 1,
            scores,
            max_depth,
            best_scores,
            false, // attenuation resumes after first hop
        )?;
    }

    Ok(())
}

// ── Step 3: Process Staleness Queue ───────────────────────────────────────────

/// Dequeues pending staleness items for the build runner to re-answer.
///
/// Returns up to `limit` items ordered by priority (highest first).
/// The actual re-answering is handled by the build runner, not this module.
pub fn process_staleness_queue(
    conn: &Connection,
    slug: &str,
    limit: u32,
) -> Result<Vec<StalenessItem>> {
    let items = db::dequeue_staleness(conn, slug, limit)?;
    info!(slug, count = items.len(), "Dequeued staleness items");
    Ok(items)
}

// ── Internal Helpers ──────────────────────────────────────────────────────────

/// Normalize a file path for consistent lookup: strip trailing slashes and
/// resolve `.` / `..` components without touching the filesystem.
fn normalize_file_path(path: &str) -> String {
    use std::path::PathBuf;
    let p = PathBuf::from(path);

    // PathBuf already handles trailing slash normalization on construction,
    // but we also want to collapse `.` and `..` segments lexically.
    // `components()` gives us normalized segments.
    let normalized: PathBuf = p.components().collect();

    // Convert back to string; fall back to original if OsStr conversion fails
    normalized.to_str().unwrap_or(path).to_string()
}

/// Look up L0 node IDs that were extracted from a given source file.
///
/// Uses the `pyramid_file_hashes` table which stores `node_ids` as a JSON array
/// keyed by `(slug, file_path)`. The path is normalized before lookup to handle
/// trailing slashes and `.`/`..` segments.
fn get_l0_node_ids_for_file(conn: &Connection, slug: &str, file_path: &str) -> Result<Vec<String>> {
    let normalized = normalize_file_path(file_path);

    let result: Result<String, _> = conn.query_row(
        "SELECT node_ids FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
        rusqlite::params![slug, &normalized],
        |row| row.get(0),
    );

    match result {
        Ok(json_str) => {
            let node_ids: Vec<String> = serde_json::from_str(&json_str).unwrap_or_default();
            Ok(node_ids)
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

// ── Unit Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Attenuation Calculation Tests ─────────────────────────────────────

    #[test]
    fn test_attenuation_single_hop() {
        // L0 changes → L1 with weight 0.95
        // Expected: L1 score = 1.0 * 0.95 = 0.95
        let mut scores: HashMap<String, f64> = HashMap::new();
        let score: f64 = 1.0 * 0.95;
        scores.insert("L1-001".to_string(), score);
        assert!((score - 0.95_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn test_attenuation_two_hops() {
        // L0→L1 weight 0.95, L1→L2 weight 0.8
        // Expected: L2 score = 1.0 * 0.95 * 0.8 = 0.76
        let l1_score: f64 = 1.0 * 0.95;
        let l2_score: f64 = l1_score * 0.8;
        assert!((l2_score - 0.76_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn test_attenuation_three_hops() {
        // L0→L1 (0.9) → L2 (0.7) → L3 (0.5)
        // Expected: L3 score = 0.9 * 0.7 * 0.5 = 0.315
        let score: f64 = 1.0 * 0.9 * 0.7 * 0.5;
        assert!((score - 0.315_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn test_threshold_filtering() {
        let threshold = 0.3;
        let scores = vec![
            ("Q1", 0.95),  // above
            ("Q2", 0.29),  // below
            ("Q3", 0.30),  // exactly at threshold (should be included)
            ("Q4", 0.001), // well below
            ("Q5", 0.76),  // above
        ];

        let affected: Vec<&str> = scores
            .iter()
            .filter(|(_, s)| *s >= threshold)
            .map(|(id, _)| *id)
            .collect();

        assert_eq!(affected, vec!["Q1", "Q3", "Q5"]);
    }

    #[test]
    fn test_max_score_wins_multiple_paths() {
        // If Q1 is reached via two paths with scores 0.76 and 0.45,
        // the max (0.76) should be kept.
        let mut scores: HashMap<String, f64> = HashMap::new();

        // Path 1: score 0.45
        let path1_score = 0.45;
        let current = scores.get("Q1").copied().unwrap_or(0.0);
        if path1_score > current {
            scores.insert("Q1".to_string(), path1_score);
        }

        // Path 2: score 0.76
        let path2_score = 0.76;
        let current = scores.get("Q1").copied().unwrap_or(0.0);
        if path2_score > current {
            scores.insert("Q1".to_string(), path2_score);
        }

        assert!((scores["Q1"] - 0.76).abs() < f64::EPSILON);
    }

    #[test]
    fn test_negligible_score_cutoff() {
        // Scores below 0.001 should be skipped
        let attenuated = 0.0005;
        assert!(attenuated < 0.001, "Should be below negligible cutoff");
    }

    #[test]
    fn test_default_weight_when_none() {
        // When evidence link has no weight, default to 1.0
        let weight: Option<f64> = None;
        let effective = weight.unwrap_or(1.0);
        assert!((effective - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_change_type_roundtrip() {
        assert_eq!(ChangeType::from_str("addition"), ChangeType::Addition);
        assert_eq!(
            ChangeType::from_str("modification"),
            ChangeType::Modification
        );
        assert_eq!(
            ChangeType::from_str("supersession"),
            ChangeType::Supersession
        );
        assert_eq!(ChangeType::from_str("deletion"), ChangeType::Deletion);
        assert_eq!(ChangeType::from_str("unknown"), ChangeType::Modification); // default
        assert_eq!(ChangeType::Addition.as_str(), "addition");
        assert_eq!(ChangeType::Modification.as_str(), "modification");
        assert_eq!(ChangeType::Supersession.as_str(), "supersession");
        assert_eq!(ChangeType::Deletion.as_str(), "deletion");
    }

    #[test]
    fn test_skip_first_attenuation() {
        assert!(!ChangeType::Addition.skip_first_attenuation());
        assert!(!ChangeType::Modification.skip_first_attenuation());
        assert!(ChangeType::Supersession.skip_first_attenuation());
        assert!(ChangeType::Deletion.skip_first_attenuation());
    }

    #[test]
    fn test_path_normalization() {
        assert_eq!(normalize_file_path("src/main.rs"), "src/main.rs");
        assert_eq!(normalize_file_path("src/./main.rs"), "src/main.rs");
        assert_eq!(normalize_file_path("src/lib/../main.rs"), "src/main.rs");
        assert_eq!(normalize_file_path("src/main.rs/"), "src/main.rs");
        assert_eq!(
            normalize_file_path("/abs/path/file.rs"),
            "/abs/path/file.rs"
        );
        assert_eq!(
            normalize_file_path("/abs/path/file.rs/"),
            "/abs/path/file.rs"
        );
    }

    #[test]
    fn test_higher_score_re_propagates_through_shared_node() {
        // Verify that best_scores allows higher scores through previously-visited nodes.
        // Simulates: L0-A reaches L1-X with score 0.3, then L0-B reaches L1-X with 0.9.
        // L1-X's upward links should use the higher score.
        let mut best_scores: HashMap<String, f64> = HashMap::new();

        // First visit: score 0.3
        let prev = best_scores.get("L1-X").copied().unwrap_or(-1.0);
        assert!(0.3 > prev, "First visit should proceed");
        best_scores.insert("L1-X".to_string(), 0.3);

        // Second visit: lower score 0.2 — should be skipped
        let prev = best_scores.get("L1-X").copied().unwrap_or(-1.0);
        assert!(0.2 <= prev, "Lower score should be skipped");

        // Third visit: higher score 0.9 — should re-propagate
        let prev = best_scores.get("L1-X").copied().unwrap_or(-1.0);
        assert!(0.9 > prev, "Higher score should re-propagate");
        best_scores.insert("L1-X".to_string(), 0.9);

        assert!((best_scores["L1-X"] - 0.9).abs() < f64::EPSILON);
    }

    // ── Integration-style test with in-memory DB ─────────────────────────

    #[test]
    fn test_propagate_staleness_with_db() {
        // Set up in-memory SQLite with the required tables
        let conn = Connection::open_in_memory().unwrap();
        setup_test_db(&conn);

        let slug = "test-pyramid";

        // Create the slug
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, 'code', '/src')",
            rusqlite::params![slug],
        ).unwrap();

        // Register file → L0 node mapping
        conn.execute(
            "INSERT INTO pyramid_file_hashes (slug, file_path, hash, chunk_count, node_ids)
             VALUES (?1, 'src/main.rs', 'abc123', 1, '[\"L0-001\", \"L0-002\"]')",
            rusqlite::params![slug],
        )
        .unwrap();

        // Create evidence links: L0-001 → L1-001 (weight 0.95)
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L0-001', 'L1-001', 'KEEP', 0.95, 'direct citation')",
            rusqlite::params![slug],
        ).unwrap();

        // L1-001 → L2-001 (weight 0.8)
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L1-001', 'L2-001', 'KEEP', 0.8, 'synthesized')",
            rusqlite::params![slug],
        ).unwrap();

        // L0-002 → L1-002 (weight 0.4) — low weight, should attenuate below threshold
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L0-002', 'L1-002', 'KEEP', 0.4, 'weak reference')",
            rusqlite::params![slug],
        ).unwrap();

        // L0-001 → L1-003 DISCONNECT — should NOT propagate
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L0-001', 'L1-003', 'DISCONNECT', NULL, 'disconnected')",
            rusqlite::params![slug],
        ).unwrap();

        // Step 1: Detect changes
        let changed = vec![ChangedFile {
            path: "src/main.rs".to_string(),
            change_type: ChangeType::Modification,
        }];
        let deltas = detect_source_changes(&conn, slug, &changed).unwrap();
        assert!(!deltas.is_empty());

        // Step 2: Propagate with threshold 0.3
        let report = propagate_staleness(&conn, slug, &deltas, 0.3).unwrap();

        // L1-001: score = 0.95 (above 0.3) ✓
        assert!(report.staleness_scores.contains_key("L1-001"));
        assert!((report.staleness_scores["L1-001"] - 0.95).abs() < f64::EPSILON);

        // L2-001: score = 0.95 * 0.8 = 0.76 (above 0.3) ✓
        assert!(report.staleness_scores.contains_key("L2-001"));
        assert!((report.staleness_scores["L2-001"] - 0.76).abs() < f64::EPSILON);

        // L1-002: score = 0.4 (above 0.3) ✓
        assert!(report.staleness_scores.contains_key("L1-002"));
        assert!((report.staleness_scores["L1-002"] - 0.4).abs() < f64::EPSILON);

        // L1-003: NOT in scores (DISCONNECT link)
        assert!(!report.staleness_scores.contains_key("L1-003"));

        // All three above threshold should be affected
        assert_eq!(report.affected_questions.len(), 3);
        assert!(report.affected_questions.contains(&"L1-001".to_string()));
        assert!(report.affected_questions.contains(&"L2-001".to_string()));
        assert!(report.affected_questions.contains(&"L1-002".to_string()));

        // max_depth should be 2 (L0 → L1 → L2)
        assert_eq!(report.max_depth_reached, 2);

        // Step 3: Dequeue — should get 3 items
        let items = process_staleness_queue(&conn, slug, 10).unwrap();
        assert_eq!(items.len(), 3);

        // Dequeue again — should be empty
        let items2 = process_staleness_queue(&conn, slug, 10).unwrap();
        assert!(items2.is_empty());

        // Source deltas should be marked processed
        let unprocessed = db::get_unprocessed_source_deltas(&conn, slug).unwrap();
        assert!(unprocessed.is_empty());
    }

    #[test]
    fn test_below_threshold_not_enqueued() {
        let conn = Connection::open_in_memory().unwrap();
        setup_test_db(&conn);

        let slug = "test-threshold";

        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, 'code', '/src')",
            rusqlite::params![slug],
        ).unwrap();

        // File maps to L0-001
        conn.execute(
            "INSERT INTO pyramid_file_hashes (slug, file_path, hash, chunk_count, node_ids)
             VALUES (?1, 'src/lib.rs', 'def456', 1, '[\"L0-001\"]')",
            rusqlite::params![slug],
        )
        .unwrap();

        // L0-001 → L1-001 with weight 0.2 (below default threshold of 0.3)
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L0-001', 'L1-001', 'KEEP', 0.2, 'weak reference')",
            rusqlite::params![slug],
        ).unwrap();

        let changed = vec![ChangedFile {
            path: "src/lib.rs".to_string(),
            change_type: ChangeType::Modification,
        }];
        let deltas = detect_source_changes(&conn, slug, &changed).unwrap();
        let report = propagate_staleness(&conn, slug, &deltas, 0.3).unwrap();

        // Score should be recorded but NOT enqueued
        assert!(report.staleness_scores.contains_key("L1-001"));
        assert!((report.staleness_scores["L1-001"] - 0.2).abs() < f64::EPSILON);
        assert!(report.affected_questions.is_empty());

        // Queue should be empty
        let items = process_staleness_queue(&conn, slug, 10).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_no_l0_nodes_for_file() {
        let conn = Connection::open_in_memory().unwrap();
        setup_test_db(&conn);

        let slug = "test-no-nodes";

        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, 'code', '/src')",
            rusqlite::params![slug],
        ).unwrap();

        // No file hash entry for "src/unknown.rs"
        let changed = vec![ChangedFile {
            path: "src/unknown.rs".to_string(),
            change_type: ChangeType::Addition,
        }];
        let deltas = detect_source_changes(&conn, slug, &changed).unwrap();
        let report = propagate_staleness(&conn, slug, &deltas, 0.3).unwrap();

        assert!(report.affected_questions.is_empty());
        assert!(report.staleness_scores.is_empty());
        assert_eq!(report.max_depth_reached, 0);
    }

    #[test]
    fn test_deletion_skips_first_attenuation() {
        // Deletion should pass full score through the first evidence link
        let conn = Connection::open_in_memory().unwrap();
        setup_test_db(&conn);

        let slug = "test-deletion";

        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, 'code', '/src')",
            rusqlite::params![slug],
        ).unwrap();

        conn.execute(
            "INSERT INTO pyramid_file_hashes (slug, file_path, hash, chunk_count, node_ids)
             VALUES (?1, 'src/deleted.rs', 'del789', 1, '[\"L0-D1\"]')",
            rusqlite::params![slug],
        )
        .unwrap();

        // L0-D1 → L1-D1 with weight 0.5 — normally would attenuate to 0.5,
        // but Deletion skips first attenuation so L1-D1 should get 1.0.
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L0-D1', 'L1-D1', 'KEEP', 0.5, 'reference')",
            rusqlite::params![slug],
        ).unwrap();

        // L1-D1 → L2-D1 with weight 0.6 — attenuation resumes, so L2-D1 = 1.0 * 0.6 = 0.6
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L1-D1', 'L2-D1', 'KEEP', 0.6, 'synthesized')",
            rusqlite::params![slug],
        ).unwrap();

        let changed = vec![ChangedFile {
            path: "src/deleted.rs".to_string(),
            change_type: ChangeType::Deletion,
        }];
        let deltas = detect_source_changes(&conn, slug, &changed).unwrap();
        let report = propagate_staleness(&conn, slug, &deltas, 0.3).unwrap();

        // L1-D1: first hop skipped attenuation → score = 1.0 (not 0.5)
        assert!((report.staleness_scores["L1-D1"] - 1.0).abs() < f64::EPSILON);

        // L2-D1: second hop attenuates normally → score = 1.0 * 0.6 = 0.6
        assert!((report.staleness_scores["L2-D1"] - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn test_shared_node_higher_score_propagates() {
        // Two L0 nodes reach the same L1 node; the higher score should propagate upward.
        let conn = Connection::open_in_memory().unwrap();
        setup_test_db(&conn);

        let slug = "test-shared";

        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, 'code', '/src')",
            rusqlite::params![slug],
        ).unwrap();

        // Two files, each mapping to a different L0 node
        conn.execute(
            "INSERT INTO pyramid_file_hashes (slug, file_path, hash, chunk_count, node_ids)
             VALUES (?1, 'src/a.rs', 'aaa', 1, '[\"L0-A\"]')",
            rusqlite::params![slug],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pyramid_file_hashes (slug, file_path, hash, chunk_count, node_ids)
             VALUES (?1, 'src/b.rs', 'bbb', 1, '[\"L0-B\"]')",
            rusqlite::params![slug],
        )
        .unwrap();

        // L0-A → L1-X with weight 0.3  (score arriving at L1-X = 0.3)
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L0-A', 'L1-X', 'KEEP', 0.3, 'ref')",
            rusqlite::params![slug],
        ).unwrap();

        // L0-B → L1-X with weight 0.9  (score arriving at L1-X = 0.9)
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L0-B', 'L1-X', 'KEEP', 0.9, 'strong ref')",
            rusqlite::params![slug],
        ).unwrap();

        // L1-X → L2-Y with weight 0.8
        // With the old shared-visited bug, L2-Y would get 0.3*0.8=0.24 if A processes first.
        // With the fix, L2-Y should get 0.9*0.8=0.72.
        conn.execute(
            "INSERT INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, 'L1-X', 'L2-Y', 'KEEP', 0.8, 'synth')",
            rusqlite::params![slug],
        ).unwrap();

        let changed = vec![
            ChangedFile {
                path: "src/a.rs".to_string(),
                change_type: ChangeType::Modification,
            },
            ChangedFile {
                path: "src/b.rs".to_string(),
                change_type: ChangeType::Modification,
            },
        ];
        let deltas = detect_source_changes(&conn, slug, &changed).unwrap();
        let report = propagate_staleness(&conn, slug, &deltas, 0.3).unwrap();

        // L1-X should have the higher score (0.9)
        assert!((report.staleness_scores["L1-X"] - 0.9).abs() < f64::EPSILON);

        // L2-Y should be 0.9 * 0.8 = 0.72, NOT 0.3 * 0.8 = 0.24
        assert!((report.staleness_scores["L2-Y"] - 0.72).abs() < f64::EPSILON);
    }

    /// Helper: create minimal tables needed for staleness tests.
    fn setup_test_db(conn: &Connection) {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS pyramid_slugs (
                slug TEXT PRIMARY KEY,
                content_type TEXT NOT NULL DEFAULT 'code',
                source_path TEXT NOT NULL DEFAULT '',
                node_count INTEGER NOT NULL DEFAULT 0,
                max_depth INTEGER NOT NULL DEFAULT 0,
                last_built_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS pyramid_file_hashes (
                slug TEXT NOT NULL,
                file_path TEXT NOT NULL,
                hash TEXT NOT NULL,
                chunk_count INTEGER NOT NULL DEFAULT 0,
                node_ids TEXT NOT NULL DEFAULT '[]',
                last_ingested_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (slug, file_path)
            );
            CREATE TABLE IF NOT EXISTS pyramid_evidence (
                slug TEXT NOT NULL,
                source_node_id TEXT NOT NULL,
                target_node_id TEXT NOT NULL,
                verdict TEXT NOT NULL,
                weight REAL,
                reason TEXT
            );
            CREATE TABLE IF NOT EXISTS pyramid_source_deltas (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slug TEXT NOT NULL,
                file_path TEXT NOT NULL,
                change_type TEXT NOT NULL,
                diff_summary TEXT,
                processed INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS pyramid_staleness_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slug TEXT NOT NULL,
                question_id TEXT NOT NULL,
                reason TEXT NOT NULL,
                channel TEXT NOT NULL,
                priority REAL NOT NULL DEFAULT 0.0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            ",
        )
        .unwrap();
    }
}
