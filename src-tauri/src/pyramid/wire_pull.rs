// pyramid/wire_pull.rs — Phase 14: Wire contribution pull flow.
//
// Pulls a Wire contribution into the local `pyramid_config_contributions`
// table with `source = "wire"`. Applies a credential safety gate that
// rejects pulls referencing undefined credentials (`${VAR_NAME}`
// references not present in the user's `.credentials` file).
//
// Per `docs/specs/config-contribution-and-wire-sharing.md` → "Pull flow"
// and `docs/specs/wire-discovery-ranking.md` → "Notifications → Pull
// latest button".

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::pyramid::config_contributions::{
    create_config_contribution_with_metadata, load_contribution_by_id,
    sync_config_to_operational, ConfigContribution,
};
use crate::pyramid::credentials::{CredentialStore, SharedCredentialStore};
use crate::pyramid::event_bus::BuildEventBus;
use crate::pyramid::wire_native_metadata::{
    default_wire_native_metadata, WireMaturity, WireNativeMetadata,
};
use crate::pyramid::wire_publish::{PyramidPublisher, WireContributionFull};

/// Result of a successful `pull_wire_contribution` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullOutcome {
    pub new_local_contribution_id: String,
    pub activated: bool,
    pub wire_contribution_id: String,
    pub schema_type: String,
    pub slug: Option<String>,
    /// Credential references found in the pulled yaml_content, if any.
    /// Empty when the safety gate passed.
    pub credential_refs_resolved: Vec<String>,
}

/// Error variants returned by the pull flow. Mapped to string errors
/// at the IPC layer; the structured variants let tests assert exact
/// failure modes.
#[derive(Debug, thiserror::Error)]
pub enum PullError {
    #[error("credential safety gate refused pull: missing credentials {0:?}")]
    MissingCredentials(Vec<String>),
    #[error("wire fetch failed: {0}")]
    FetchFailed(String),
    #[error("pulled contribution has empty yaml_content")]
    EmptyPayload,
    #[error("pulled contribution missing schema_type")]
    MissingSchemaType,
    #[error("database error: {0}")]
    DbError(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Scan the pulled YAML for `${VAR_NAME}` references and verify each
/// one is defined in the user's credential store. Returns the full
/// list of referenced variable names so the caller can include it in
/// the outcome for observability.
///
/// Returns `PullError::MissingCredentials` when any reference is
/// undefined — the user must create the variable in their
/// `.credentials` file before retrying the pull. The error lists
/// exactly which variables are missing so the UI can surface them.
pub fn credential_safety_gate(
    yaml_content: &str,
    credential_store: &SharedCredentialStore,
) -> Result<Vec<String>, PullError> {
    let references = CredentialStore::collect_references(yaml_content);
    if references.is_empty() {
        return Ok(Vec::new());
    }

    // `SharedCredentialStore = Arc<CredentialStore>`. The store uses
    // internal RwLocks — `contains()` takes `&self`.
    let mut missing: Vec<String> = Vec::new();
    for var_name in &references {
        if !credential_store.contains(var_name) {
            missing.push(var_name.clone());
        }
    }

    if !missing.is_empty() {
        return Err(PullError::MissingCredentials(missing));
    }

    Ok(references)
}

/// Construct a `WireNativeMetadata` for a pulled contribution from
/// the Wire's response. Uses the default mapping-table metadata as the
/// base, then overrides with Wire-provided fields. The maturity is
/// reset to `Draft` so the user reviews the pulled contribution before
/// it can be re-published.
fn build_metadata_from_pulled(full: &WireContributionFull, slug: Option<&str>) -> WireNativeMetadata {
    let schema_type = full.schema_type.as_deref().unwrap_or("unknown");
    let mut metadata = default_wire_native_metadata(schema_type, slug);
    metadata.maturity = WireMaturity::Draft;
    for tag in &full.tags {
        if !metadata.topics.iter().any(|t| t == tag) {
            metadata.topics.push(tag.clone());
        }
    }
    metadata
}

/// Options for a pull. Separated from the main function signature so
/// the caller can extend without a breaking change.
#[derive(Debug, Clone)]
pub struct PullOptions<'a> {
    /// Wire contribution ID to pull (the latest version to activate).
    pub latest_wire_contribution_id: &'a str,
    /// Optional prior local contribution to supersede when `activate = true`.
    /// When `None`, the pulled contribution lands as a brand-new active row.
    pub local_contribution_id_to_supersede: Option<&'a str>,
    /// When true, immediately activate the pulled contribution and
    /// supersede the prior local version (if any). Otherwise the row
    /// lands as `status = 'proposed'` for manual review.
    pub activate: bool,
    /// Optional slug. When `None`, the contribution is treated as global
    /// (same semantics as the active-config lookup).
    pub slug: Option<&'a str>,
}

/// Pull a Wire contribution into the local contribution store.
///
/// Flow (per the spec):
/// 1. Fetch the full contribution payload from the Wire.
/// 2. Apply the credential safety gate — refuse pulls that reference
///    undefined credentials.
/// 3. Build a new local contribution row with `source = "wire"` and
///    `wire_contribution_id` set.
/// 4. If `activate = true`, mark it active and sync to operational tables.
///    If there's a prior local contribution to supersede, mark that row
///    as superseded first.
/// 5. Return the new local contribution_id so the caller can delete the
///    corresponding `pyramid_wire_update_cache` row.
pub async fn pull_wire_contribution(
    conn: &mut Connection,
    publisher: &PyramidPublisher,
    credential_store: &SharedCredentialStore,
    bus: &Arc<BuildEventBus>,
    options: PullOptions<'_>,
) -> Result<PullOutcome, PullError> {
    // Step 1: fetch the full contribution from the Wire.
    let full = publisher
        .fetch_contribution(options.latest_wire_contribution_id)
        .await
        .map_err(|e| PullError::FetchFailed(e.to_string()))?;

    if full.yaml_content.trim().is_empty() {
        return Err(PullError::EmptyPayload);
    }
    let schema_type = full
        .schema_type
        .as_deref()
        .ok_or(PullError::MissingSchemaType)?
        .to_string();

    // Step 2: credential safety gate. Refuses pulls that reference
    // undefined credentials.
    let credential_refs = credential_safety_gate(&full.yaml_content, credential_store)?;

    // Step 3: build a new local contribution row with source = "wire".
    let metadata = build_metadata_from_pulled(&full, options.slug);

    // Decide the initial status: active iff activate=true AND no
    // superseding chain was requested. Otherwise the row lands as
    // 'proposed' so the user reviews it via the normal pending-proposals
    // surface.
    let initial_status = if options.activate {
        "active"
    } else {
        "proposed"
    };

    let triggering_note = if options.activate {
        format!(
            "Pulled from Wire ({})",
            options.latest_wire_contribution_id.chars().take(8).collect::<String>()
        )
    } else {
        format!(
            "Pulled from Wire for review ({})",
            options.latest_wire_contribution_id.chars().take(8).collect::<String>()
        )
    };

    // If we're activating AND superseding, do the full supersession
    // transaction (mark prior superseded + insert new active +
    // supersedes_id pointer). Otherwise just insert a fresh row.
    let new_id = if options.activate {
        if let Some(prior_id) = options.local_contribution_id_to_supersede {
            // Ensure the prior row exists before we touch anything.
            let prior = load_contribution_by_id(conn, prior_id)
                .map_err(|e| PullError::Other(e))?
                .ok_or_else(|| {
                    PullError::Other(anyhow!(
                        "prior local contribution {prior_id} not found — cannot supersede"
                    ))
                })?;
            supersede_with_pulled(
                conn,
                &prior,
                &full.yaml_content,
                &triggering_note,
                &metadata,
                options.latest_wire_contribution_id,
            )?
        } else {
            insert_pulled_contribution(
                conn,
                &schema_type,
                options.slug,
                &full.yaml_content,
                &triggering_note,
                &metadata,
                options.latest_wire_contribution_id,
                initial_status,
            )?
        }
    } else {
        insert_pulled_contribution(
            conn,
            &schema_type,
            options.slug,
            &full.yaml_content,
            &triggering_note,
            &metadata,
            options.latest_wire_contribution_id,
            initial_status,
        )?
    };

    // Step 4: if activating, sync to operational tables so the executor
    // sees the new values on its next read. We only sync when the row
    // lands as 'active' — proposed contributions don't feed operational
    // tables until the user accepts them.
    let activated = options.activate;
    if activated {
        let contribution = load_contribution_by_id(conn, &new_id)
            .map_err(|e| PullError::Other(e))?
            .ok_or_else(|| {
                PullError::Other(anyhow!(
                    "contribution {new_id} disappeared immediately after insert"
                ))
            })?;
        if let Err(e) = sync_config_to_operational(conn, bus, &contribution) {
            tracing::warn!(
                contribution_id = %new_id,
                error = %e,
                "sync_config_to_operational failed after wire pull"
            );
        }
    }

    Ok(PullOutcome {
        new_local_contribution_id: new_id,
        activated,
        wire_contribution_id: options.latest_wire_contribution_id.to_string(),
        schema_type,
        slug: options.slug.map(|s| s.to_string()),
        credential_refs_resolved: credential_refs,
    })
}

/// Insert a pulled contribution row without superseding any prior
/// version. Used for the "fresh pull" path where the user doesn't
/// already have a local version of this schema_type.
fn insert_pulled_contribution(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
    yaml_content: &str,
    triggering_note: &str,
    metadata: &WireNativeMetadata,
    wire_contribution_id: &str,
    status: &str,
) -> Result<String, PullError> {
    let new_id = create_config_contribution_with_metadata(
        conn,
        schema_type,
        slug,
        yaml_content,
        Some(triggering_note),
        "wire",
        Some("wire-pull"),
        status,
        metadata,
    )
    .map_err(|e| PullError::Other(e))?;

    // create_config_contribution_with_metadata doesn't set
    // wire_contribution_id — we patch it in after the insert.
    conn.execute(
        "UPDATE pyramid_config_contributions
         SET wire_contribution_id = ?1
         WHERE contribution_id = ?2",
        rusqlite::params![wire_contribution_id, new_id],
    )?;

    Ok(new_id)
}

/// Supersede a prior local contribution with a pulled version.
/// Atomic transaction: marks the prior as superseded + inserts the
/// new active row with `supersedes_id` pointing at the prior +
/// `wire_contribution_id` set to the pulled ID.
fn supersede_with_pulled(
    conn: &mut Connection,
    prior: &ConfigContribution,
    new_yaml_content: &str,
    triggering_note: &str,
    metadata: &WireNativeMetadata,
    wire_contribution_id: &str,
) -> Result<String, PullError> {
    let metadata_json = metadata
        .to_json()
        .map_err(|e| PullError::Other(anyhow!("serialize metadata: {e}")))?;

    let tx = conn.transaction()?;

    let new_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            wire_native_metadata_json, wire_publication_state_json,
            supersedes_id, superseded_by_id, triggering_note,
            status, source, wire_contribution_id, created_by, accepted_at
         ) VALUES (
            ?1, ?2, ?3, ?4,
            ?5, '{}',
            ?6, NULL, ?7,
            'active', 'wire', ?8, 'wire-pull', datetime('now')
         )",
        rusqlite::params![
            new_id,
            prior.slug,
            prior.schema_type,
            new_yaml_content,
            metadata_json,
            prior.contribution_id,
            triggering_note,
            wire_contribution_id,
        ],
    )?;

    tx.execute(
        "UPDATE pyramid_config_contributions
         SET status = 'superseded', superseded_by_id = ?1
         WHERE contribution_id = ?2",
        rusqlite::params![new_id, prior.contribution_id],
    )?;

    tx.commit()?;
    Ok(new_id)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod phase14_tests {
    use super::*;
    use crate::pyramid::credentials::CredentialStore;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_store_with(vars: &[(&str, &str)]) -> (TempDir, SharedCredentialStore) {
        let tmp = TempDir::new().expect("tempdir");
        // Load from a non-existent path — load_from_path returns an
        // empty store when the file doesn't exist. Then set() the
        // test variables; set() writes through to disk.
        let store = CredentialStore::load_from_path(tmp.path().join(".credentials"))
            .expect("load empty credentials store");
        for (k, v) in vars {
            store.set(k, v).expect("set credential");
        }
        (tmp, Arc::new(store))
    }

    #[test]
    fn test_credential_safety_gate_passes_when_all_refs_defined() {
        let (_tmp, store) = make_store_with(&[("OPENROUTER_API_KEY", "sk-xxx")]);
        let yaml = "schema_type: custom_prompts\napi_key: ${OPENROUTER_API_KEY}\n";
        let refs = credential_safety_gate(yaml, &store).unwrap();
        assert_eq!(refs, vec!["OPENROUTER_API_KEY".to_string()]);
    }

    #[test]
    fn test_credential_safety_gate_rejects_missing_credentials() {
        let (_tmp, store) = make_store_with(&[("DEFINED_VAR", "value")]);
        let yaml = "schema_type: custom_prompts\napi_key: ${MISSING_VAR}\nanother: ${DEFINED_VAR}\n";
        let result = credential_safety_gate(yaml, &store);
        match result {
            Err(PullError::MissingCredentials(missing)) => {
                assert_eq!(missing, vec!["MISSING_VAR".to_string()]);
            }
            other => panic!("expected MissingCredentials, got {:?}", other),
        }
    }

    #[test]
    fn test_credential_safety_gate_empty_refs_passes() {
        let (_tmp, store) = make_store_with(&[]);
        let yaml = "schema_type: custom_prompts\nextraction_focus: plain text\n";
        let refs = credential_safety_gate(yaml, &store).unwrap();
        assert!(refs.is_empty());
    }
}
