// pyramid/chain_proposal.rs — WS-CHAIN-PROPOSAL
//
// Agents propose updates to chain configurations based on what they learn
// during sessions. Proposals are stored as contributions and surface to the
// operator for review. This closes the learning loop — the substrate
// accumulates improvements to how content gets processed.
//
// Submit flow:
//   1. Agent calls submit_chain_proposal with a patch + reasoning
//   2. Proposal is stored in pyramid_chain_proposals with status = 'pending'
//   3. ChainProposalReceived event is emitted on the build event bus
//
// Accept flow:
//   1. Operator accepts via HTTP
//   2. apply_accepted_proposal loads the chain YAML, merges the patch,
//      saves with incremented version, records in chain_publications

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::chain_loader::discover_chains;
use super::db;
use super::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};
use super::types::ChainProposal;

/// Submit a new chain proposal from an agent. Creates the DB record and emits
/// a `ChainProposalReceived` event on the build event bus.
///
/// Returns the generated proposal_id (UUID string).
pub fn submit_chain_proposal(
    conn: &Connection,
    chain_id: &str,
    proposer: &str,
    proposal_type: &str,
    description: &str,
    reasoning: &str,
    patch: &serde_json::Value,
    bus: Option<&BuildEventBus>,
) -> Result<String> {
    let proposal_id = uuid::Uuid::new_v4().to_string();

    let proposal = ChainProposal {
        id: 0, // Will be set by DB
        proposal_id: proposal_id.clone(),
        chain_id: chain_id.to_string(),
        proposer: proposer.to_string(),
        proposal_type: proposal_type.to_string(),
        description: description.to_string(),
        reasoning: reasoning.to_string(),
        patch: patch.clone(),
        status: "pending".to_string(),
        operator_notes: None,
        created_at: String::new(), // DB default
        reviewed_at: None,
    };

    let row_id = db::create_chain_proposal(conn, &proposal)?;

    // Emit event on the build event bus
    if let Some(bus) = bus {
        let _ = bus.tx.send(TaggedBuildEvent {
            slug: chain_id.to_string(),
            kind: TaggedKind::ChainProposalReceived {
                chain_id: chain_id.to_string(),
                proposal_id: row_id,
            },
        });
    }

    tracing::info!(
        proposal_id = %proposal_id,
        chain_id = chain_id,
        proposer = proposer,
        proposal_type = proposal_type,
        "chain proposal submitted"
    );

    Ok(proposal_id)
}

/// Apply an accepted proposal to the chain YAML on disk.
///
/// Loads the chain YAML, deep-merges the proposal's patch into it, bumps the
/// version string, writes the modified YAML back, and records the new version
/// in `pyramid_chain_publications`.
pub fn apply_accepted_proposal(proposal: &ChainProposal, chains_dir: &str) -> Result<()> {
    let chains_path = std::path::Path::new(chains_dir);

    // Find the chain YAML file on disk
    let chain_file = find_chain_file(chains_path, &proposal.chain_id)?;

    // Read and parse to a serde_json::Value for patching
    let raw_yaml = std::fs::read_to_string(&chain_file)
        .with_context(|| format!("failed to read chain file: {}", chain_file.display()))?;

    let mut chain_value: serde_json::Value = serde_yaml::from_str(&raw_yaml)
        .with_context(|| format!("failed to parse chain YAML: {}", chain_file.display()))?;

    // Deep-merge the patch into the chain definition
    deep_merge(&mut chain_value, &proposal.patch);

    // Bump the version string (append .1 or increment trailing number)
    if let Some(version) = chain_value.get("version").and_then(|v| v.as_str()) {
        let new_version = increment_version_string(version);
        chain_value["version"] = serde_json::Value::String(new_version);
    }

    // Write the modified YAML back
    let new_yaml =
        serde_yaml::to_string(&chain_value).context("failed to serialize patched chain YAML")?;
    std::fs::write(&chain_file, &new_yaml)
        .with_context(|| format!("failed to write patched chain: {}", chain_file.display()))?;

    tracing::info!(
        proposal_id = %proposal.proposal_id,
        chain_id = %proposal.chain_id,
        chain_file = %chain_file.display(),
        "applied accepted chain proposal"
    );

    Ok(())
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Find a chain YAML file by chain_id. Searches defaults/ then variants/.
fn find_chain_file(chains_dir: &std::path::Path, chain_id: &str) -> Result<std::path::PathBuf> {
    let chains = discover_chains(chains_dir).context("failed to discover chains")?;

    for meta in &chains {
        if meta.id == chain_id {
            return Ok(std::path::PathBuf::from(&meta.file_path));
        }
    }

    anyhow::bail!(
        "chain '{}' not found in chains directory '{}'",
        chain_id,
        chains_dir.display()
    )
}

/// Deep-merge `patch` into `target`. Object keys in patch override or add to
/// target. Non-object values replace outright.
fn deep_merge(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(t), serde_json::Value::Object(p)) => {
            for (key, value) in p {
                if let Some(existing) = t.get_mut(key) {
                    deep_merge(existing, value);
                } else {
                    t.insert(key.clone(), value.clone());
                }
            }
        }
        (target, patch) => {
            *target = patch.clone();
        }
    }
}

/// Increment a semver-like version string. "1.0.0" -> "1.0.1", "0.2" -> "0.3".
fn increment_version_string(version: &str) -> String {
    let parts: Vec<&str> = version.rsplitn(2, '.').collect();
    if parts.len() == 2 {
        if let Ok(last_num) = parts[0].parse::<u32>() {
            return format!("{}.{}", parts[1], last_num + 1);
        }
    }
    // Fallback: append .1
    format!("{}.1", version)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_submit_proposal_is_queryable() {
        let conn = setup_test_db();

        let proposal_id = submit_chain_proposal(
            &conn,
            "conversation-default",
            "agent-session-123",
            "question_addition",
            "Add a question about error handling patterns",
            "During analysis I noticed the chain misses error handling context",
            &serde_json::json!({
                "steps": [{"new_question": "What error handling patterns are used?"}]
            }),
            None, // no bus in test
        )
        .unwrap();

        // Verify it's queryable by proposal_id
        let retrieved = db::get_chain_proposal(&conn, &proposal_id)
            .unwrap()
            .expect("proposal should be queryable");

        assert_eq!(retrieved.proposal_id, proposal_id);
        assert_eq!(retrieved.chain_id, "conversation-default");
        assert_eq!(retrieved.proposer, "agent-session-123");
        assert_eq!(retrieved.proposal_type, "question_addition");
        assert_eq!(retrieved.status, "pending");
        assert!(retrieved.reviewed_at.is_none());

        // Verify patch round-trips
        assert_eq!(
            retrieved.patch["steps"][0]["new_question"],
            "What error handling patterns are used?"
        );
    }

    #[test]
    fn test_accept_proposal_changes_status_and_sets_reviewed_at() {
        let conn = setup_test_db();

        let proposal_id = submit_chain_proposal(
            &conn,
            "code-default",
            "agent-session-456",
            "vocabulary_promotion",
            "Promote 'async-runtime' to vocabulary",
            "Frequently referenced but not in the vocabulary set",
            &serde_json::json!({"vocabulary": {"add": ["async-runtime"]}}),
            None,
        )
        .unwrap();

        // Accept the proposal
        db::accept_chain_proposal(&conn, &proposal_id, Some("Looks good, merging")).unwrap();

        let accepted = db::get_chain_proposal(&conn, &proposal_id)
            .unwrap()
            .expect("proposal should exist");

        assert_eq!(accepted.status, "accepted");
        assert!(accepted.reviewed_at.is_some(), "reviewed_at should be set");
        assert_eq!(
            accepted.operator_notes.as_deref(),
            Some("Looks good, merging")
        );
    }

    #[test]
    fn test_list_proposals_with_status_filter() {
        let conn = setup_test_db();

        // Submit 3 proposals: 2 for chain A, 1 for chain B
        let id_a1 = submit_chain_proposal(
            &conn,
            "chain-a",
            "agent-1",
            "prompt_emphasis",
            "Emphasize conciseness",
            "Output too verbose",
            &serde_json::json!({"emphasis": "concise"}),
            None,
        )
        .unwrap();

        let _id_a2 = submit_chain_proposal(
            &conn,
            "chain-a",
            "agent-2",
            "layer_rule",
            "Add layer rule for depth 3",
            "Missing intermediate synthesis",
            &serde_json::json!({"layer_rules": {"depth_3": "synthesize"}}),
            None,
        )
        .unwrap();

        let _id_b1 = submit_chain_proposal(
            &conn,
            "chain-b",
            "agent-3",
            "other",
            "Misc tweak",
            "Just a test",
            &serde_json::json!({"misc": true}),
            None,
        )
        .unwrap();

        // Accept one of chain-a's proposals
        db::accept_chain_proposal(&conn, &id_a1, None).unwrap();

        // List all pending
        let pending = db::list_chain_proposals(&conn, None, Some("pending")).unwrap();
        assert_eq!(pending.len(), 2, "should have 2 pending proposals");

        // List chain-a only
        let chain_a_all = db::list_chain_proposals(&conn, Some("chain-a"), None).unwrap();
        assert_eq!(
            chain_a_all.len(),
            2,
            "chain-a should have 2 proposals total"
        );

        // List chain-a pending only
        let chain_a_pending =
            db::list_chain_proposals(&conn, Some("chain-a"), Some("pending")).unwrap();
        assert_eq!(
            chain_a_pending.len(),
            1,
            "chain-a should have 1 pending proposal"
        );
    }

    #[test]
    fn test_reject_proposal_is_idempotent_on_already_rejected() {
        let conn = setup_test_db();

        let proposal_id = submit_chain_proposal(
            &conn,
            "conversation-default",
            "agent-session-789",
            "question_addition",
            "Add question about testing",
            "Testing is important",
            &serde_json::json!({"question": "How are tests structured?"}),
            None,
        )
        .unwrap();

        // Reject once
        db::reject_chain_proposal(&conn, &proposal_id, Some("Not relevant")).unwrap();

        let rejected = db::get_chain_proposal(&conn, &proposal_id)
            .unwrap()
            .expect("proposal should exist");
        assert_eq!(rejected.status, "rejected");
        let first_reviewed_at = rejected.reviewed_at.clone();

        // Reject again — should succeed (idempotent)
        db::reject_chain_proposal(&conn, &proposal_id, Some("Still not relevant")).unwrap();

        let re_rejected = db::get_chain_proposal(&conn, &proposal_id)
            .unwrap()
            .expect("proposal should exist");
        assert_eq!(re_rejected.status, "rejected");
        // reviewed_at should be preserved from first rejection (COALESCE)
        assert_eq!(re_rejected.reviewed_at, first_reviewed_at);
    }

    #[test]
    fn test_increment_version_string() {
        assert_eq!(increment_version_string("1.0.0"), "1.0.1");
        assert_eq!(increment_version_string("0.2"), "0.3");
        assert_eq!(increment_version_string("2.1.5"), "2.1.6");
        assert_eq!(increment_version_string("foo"), "foo.1");
    }

    #[test]
    fn test_deep_merge() {
        let mut target = serde_json::json!({
            "a": 1,
            "b": {"c": 2, "d": 3},
            "e": "keep"
        });
        let patch = serde_json::json!({
            "b": {"c": 99, "f": 4},
            "g": "new"
        });
        deep_merge(&mut target, &patch);
        assert_eq!(target["a"], 1);
        assert_eq!(target["b"]["c"], 99);
        assert_eq!(target["b"]["d"], 3);
        assert_eq!(target["b"]["f"], 4);
        assert_eq!(target["e"], "keep");
        assert_eq!(target["g"], "new");
    }
}
