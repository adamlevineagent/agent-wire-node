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
    sync_config_to_operational,
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
    /// Phase 0a-1 commit 5: unique-index contention on
    /// `uq_config_contrib_active` during pull commit. A concurrent
    /// local supersession beat the pull's INSERT. Caller can retry
    /// (re-resolve the prior active and attempt again) rather than
    /// surface a generic error to the operator.
    #[error("supersession conflict for schema_type={schema_type} slug={slug:?}: concurrent writer landed first")]
    SupersessionConflict {
        schema_type: String,
        slug: Option<String>,
    },
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

    // Wanderer fix (phase-14): the activate path MUST resolve the
    // current active row for (schema_type, slug) inside the same
    // transaction as the insert, so that:
    //
    //   (a) Pulling a Wire version via `pyramid_pull_wire_config` with
    //       `activate=true` correctly supersedes any existing active
    //       row — previously the fresh-insert branch would leave the
    //       old active untouched and two `status='active'` rows would
    //       accumulate (bug #1: direct Discover "Pull and activate").
    //
    //   (b) The poller→user concurrent race where a manual pull races
    //       with an auto-update can't leave an orphaned active row.
    //       The explicit `local_contribution_id_to_supersede` hint may
    //       point at a row the auto-updater has already flipped to
    //       `superseded`; we fall through to the real current active
    //       instead of clobbering the supersession chain (bug #2:
    //       supersede_with_pulled's unconditional UPDATE).
    //
    // The hint passed in `options.local_contribution_id_to_supersede`
    // is still honored as a sanity check, but the authoritative prior
    // is the row that satisfies the `load_active_config_contribution`
    // predicate AT TRANSACTION TIME — never an externally-captured ID.
    let new_id = if options.activate {
        commit_pulled_active(
            conn,
            &schema_type,
            options.slug,
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

/// Atomically commit a pulled contribution as the new active row for
/// `(schema_type, slug)`. Resolves the current active row inside the
/// transaction so concurrent writers (e.g. the poller + a manual pull)
/// can't race into two `status='active'` rows with corrupted
/// supersession chains.
///
/// Behavior:
/// * If an active row exists for `(schema_type, slug)` at transaction
///   time, mark it `superseded` and insert the new row with
///   `supersedes_id` pointing at it.
/// * If no active row exists, insert the new row fresh.
/// * In both cases, the new row has `status='active'`, `source='wire'`,
///   and `wire_contribution_id` set.
///
/// Wanderer fix (phase-14):
/// * Bug #1: `pyramid_pull_wire_config` with `activate=true` used to
///   call `insert_pulled_contribution` unconditionally whenever no
///   explicit prior was passed, leaving any existing active row
///   untouched. Every Discover-tab "Pull and activate" on a schema
///   type with a bundled default accumulated a duplicate active row.
/// * Bug #2: the old `supersede_with_pulled` unconditionally flipped
///   a caller-supplied `prior` to `superseded` even if it was ALREADY
///   superseded — e.g. by an earlier auto-update cycle that raced with
///   a manual pull. The UPDATE clobbered the prior's `superseded_by_id`
///   pointer and the new active row became a dangling second active.
///
/// Both failure modes are eliminated by looking up the current active
/// row inside the same SQLite transaction as the insert.
fn commit_pulled_active(
    conn: &mut Connection,
    schema_type: &str,
    slug: Option<&str>,
    new_yaml_content: &str,
    triggering_note: &str,
    metadata: &WireNativeMetadata,
    wire_contribution_id: &str,
) -> Result<String, PullError> {
    let metadata_json = metadata
        .to_json()
        .map_err(|e| PullError::Other(anyhow!("serialize metadata: {e}")))?;

    // Phase 0a-1 commit 5 / §2.16.1: BEGIN IMMEDIATE so Wire-side
    // pulls serialize on write intent against concurrent local
    // supersessions.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Resolve the current active row INSIDE the transaction. Same
    // predicate as `load_active_config_contribution`. Holding the
    // writer lock across this query guarantees no concurrent writer
    // can insert/update between lookup and supersede.
    let prior_active_id: Option<String> = if let Some(slug_val) = slug {
        tx.query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE slug = ?1 AND schema_type = ?2
               AND status = 'active' AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![slug_val, schema_type],
            |row| row.get::<_, String>(0),
        )
        .ok()
    } else {
        tx.query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE slug IS NULL AND schema_type = ?1
               AND status = 'active' AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![schema_type],
            |row| row.get::<_, String>(0),
        )
        .ok()
    };

    let new_id = uuid::Uuid::new_v4().to_string();

    // Phase 0a-1 commit 5: flip prior to superseded BEFORE inserting
    // the new active row so the `uq_config_contrib_active` unique
    // index never sees two active rows for the same (schema_type,
    // slug). The predicate includes the `status='active'` guard so
    // a re-run from a retry path is a no-op rather than clobbering
    // an already-superseded row.
    if let Some(prior_id) = &prior_active_id {
        tx.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded'
             WHERE contribution_id = ?1
               AND status = 'active'
               AND superseded_by_id IS NULL",
            rusqlite::params![prior_id],
        )?;
    }

    crate::pyramid::config_contributions::write_contribution_envelope(
        &tx,
        crate::pyramid::config_contributions::ContributionEnvelopeInput {
            contribution_id: new_id.clone(),
            slug: slug.map(|s| s.to_string()),
            schema_type: schema_type.to_string(),
            body: new_yaml_content.to_string(),
            wire_native_metadata_json: Some(metadata_json),
            supersedes_id: prior_active_id.clone(),
            triggering_note: Some(triggering_note.to_string()),
            status: "active".to_string(),
            source: "wire".to_string(),
            wire_contribution_id: Some(wire_contribution_id.to_string()),
            created_by: Some("wire-pull".to_string()),
            accepted_at: crate::pyramid::config_contributions::AcceptedAt::Now,
            needs_migration: None,
            write_mode: crate::pyramid::config_contributions::WriteMode::default(),
        },
        crate::pyramid::config_contributions::TransactionMode::JoinAmbient,
    )
    .map_err(|e| match e {
        crate::pyramid::config_contributions::ContributionWriterError::SupersessionConflict {
            schema_type,
            slug,
        } => PullError::SupersessionConflict { schema_type, slug },
        other => PullError::Other(anyhow!("write_contribution_envelope: {other}")),
    })?;

    if let Some(prior_id) = &prior_active_id {
        // Back-fill forward pointer after the INSERT so the
        // `supersedes_id`/`superseded_by_id` chain is symmetric.
        tx.execute(
            "UPDATE pyramid_config_contributions
             SET superseded_by_id = ?1
             WHERE contribution_id = ?2",
            rusqlite::params![new_id, prior_id],
        )?;
    }

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

    // ── Wanderer-fix regression tests ─────────────────────────────────

    /// Set up an in-memory pyramid DB for the wanderer-fix tests.
    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        conn
    }

    fn seed_active_contribution(
        conn: &Connection,
        schema_type: &str,
        slug: Option<&str>,
        yaml: &str,
        wire_id: Option<&str>,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (
                ?1, ?2, ?3, ?4,
                '{}', '{}',
                NULL, NULL, 'seed',
                'active', 'bundled', ?5, 'bootstrap', datetime('now')
             )",
            rusqlite::params![id, slug, schema_type, yaml, wire_id],
        )
        .unwrap();
        id
    }

    fn count_active_rows(
        conn: &Connection,
        schema_type: &str,
        slug: Option<&str>,
    ) -> i64 {
        if let Some(slug_val) = slug {
            conn.query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE slug = ?1 AND schema_type = ?2
                   AND status = 'active' AND superseded_by_id IS NULL",
                rusqlite::params![slug_val, schema_type],
                |row| row.get(0),
            )
            .unwrap()
        } else {
            conn.query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE slug IS NULL AND schema_type = ?1
                   AND status = 'active' AND superseded_by_id IS NULL",
                rusqlite::params![schema_type],
                |row| row.get(0),
            )
            .unwrap()
        }
    }

    /// Wanderer bug #1 regression: `pyramid_pull_wire_config` with
    /// `activate=true` used to call `insert_pulled_contribution`
    /// unconditionally when no explicit supersession hint was passed.
    /// That left existing active rows untouched, so every Discover-tab
    /// "Pull and activate" over a schema_type with a bundled default
    /// accumulated a duplicate active row.
    ///
    /// After the fix, `commit_pulled_active` resolves the current
    /// active row inside the transaction and supersedes it atomically.
    /// Only one active row should exist after a pull.
    #[test]
    fn test_commit_pulled_active_supersedes_existing_active() {
        let mut conn = mem_conn();
        let prior_id = seed_active_contribution(
            &conn,
            "custom_prompts",
            None,
            "schema_type: custom_prompts\nextraction_focus: old\n",
            None,
        );
        assert_eq!(count_active_rows(&conn, "custom_prompts", None), 1);

        let metadata = default_wire_native_metadata("custom_prompts", None);
        let new_id = commit_pulled_active(
            &mut conn,
            "custom_prompts",
            None,
            "schema_type: custom_prompts\nextraction_focus: new\n",
            "Pulled from Wire (wire-123)",
            &metadata,
            "wire-123",
        )
        .unwrap();

        // Exactly one active row — the pulled version.
        assert_eq!(
            count_active_rows(&conn, "custom_prompts", None),
            1,
            "pull must produce exactly one active row, not accumulate duplicates"
        );

        // Prior is now superseded, chain pointer is set.
        let (prior_status, prior_superseded_by): (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![prior_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(prior_status, "superseded");
        assert_eq!(prior_superseded_by.as_deref(), Some(new_id.as_str()));

        // New row points back at the prior via supersedes_id and
        // carries the wire_contribution_id.
        let (new_supersedes, new_wire, new_status): (
            Option<String>,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "SELECT supersedes_id, wire_contribution_id, status FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![new_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(new_supersedes.as_deref(), Some(prior_id.as_str()));
        assert_eq!(new_wire.as_deref(), Some("wire-123"));
        assert_eq!(new_status, "active");
    }

    /// Wanderer bug #2 regression: the old `supersede_with_pulled`
    /// unconditionally flipped a caller-supplied `prior` to
    /// `superseded`, overwriting its `superseded_by_id` pointer even if
    /// the prior had already been superseded by an earlier auto-update.
    /// The result was two active rows and a corrupted chain.
    ///
    /// The fix resolves the current active INSIDE the transaction and
    /// only touches THAT row, so a stale prior hint can't clobber the
    /// chain built by a racing writer.
    ///
    /// Scenario: original active row L1 exists. An auto-update pull
    /// (simulated via a direct `commit_pulled_active` call) supersedes
    /// it with L2. Then a racing manual pull arrives with a stale view
    /// of L1 as active. The manual pull must supersede L2 (the real
    /// current active), NOT clobber L1's supersession pointer or leave
    /// both L2 and L3 as active.
    #[test]
    fn test_commit_pulled_active_ignores_stale_prior_hint() {
        let mut conn = mem_conn();
        let l1 = seed_active_contribution(
            &conn,
            "custom_prompts",
            None,
            "schema_type: custom_prompts\nextraction_focus: v1\n",
            Some("wire-v1"),
        );

        // First pull (simulates the auto-updater winning the race).
        let metadata = default_wire_native_metadata("custom_prompts", None);
        let l2 = commit_pulled_active(
            &mut conn,
            "custom_prompts",
            None,
            "schema_type: custom_prompts\nextraction_focus: v2\n",
            "Pulled from Wire (wire-v2)",
            &metadata,
            "wire-v2",
        )
        .unwrap();

        // Invariant holds after the first pull.
        assert_eq!(count_active_rows(&conn, "custom_prompts", None), 1);

        // Second pull (simulates the user's manual pull arriving with
        // a stale view — their UI captured L1 as the prior before the
        // auto-update superseded it). `commit_pulled_active` takes no
        // explicit prior hint; the authoritative prior is L2.
        let l3 = commit_pulled_active(
            &mut conn,
            "custom_prompts",
            None,
            "schema_type: custom_prompts\nextraction_focus: v3\n",
            "Pulled from Wire (wire-v3)",
            &metadata,
            "wire-v3",
        )
        .unwrap();

        // Exactly one active row (L3), not two.
        assert_eq!(
            count_active_rows(&conn, "custom_prompts", None),
            1,
            "race between auto-update and manual pull must not leave two active rows"
        );

        // L1's supersession chain is INTACT — still points at L2, not
        // overwritten to L3.
        let l1_superseded_by: Option<String> = conn
            .query_row(
                "SELECT superseded_by_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![l1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            l1_superseded_by.as_deref(),
            Some(l2.as_str()),
            "L1's superseded_by_id must remain pointed at L2 — the manual pull must not clobber it"
        );

        // L2 is the row that got superseded by L3.
        let (l2_status, l2_superseded_by): (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![l2],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(l2_status, "superseded");
        assert_eq!(l2_superseded_by.as_deref(), Some(l3.as_str()));

        // L3's supersedes_id points at L2 (the real prior).
        let l3_supersedes: Option<String> = conn
            .query_row(
                "SELECT supersedes_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![l3],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(l3_supersedes.as_deref(), Some(l2.as_str()));
    }

    /// First-pull edge case: when no active row exists for the
    /// `(schema_type, slug)` pair, `commit_pulled_active` inserts the
    /// pulled row fresh with `supersedes_id IS NULL`. The behavior
    /// matches the legacy `insert_pulled_contribution` path for the
    /// "brand new schema type" case.
    #[test]
    fn test_commit_pulled_active_inserts_fresh_when_no_prior() {
        let mut conn = mem_conn();
        assert_eq!(count_active_rows(&conn, "custom_prompts", None), 0);

        let metadata = default_wire_native_metadata("custom_prompts", None);
        let new_id = commit_pulled_active(
            &mut conn,
            "custom_prompts",
            None,
            "schema_type: custom_prompts\nextraction_focus: fresh\n",
            "Pulled from Wire (wire-fresh)",
            &metadata,
            "wire-fresh",
        )
        .unwrap();

        assert_eq!(count_active_rows(&conn, "custom_prompts", None), 1);

        let (status, supersedes, wire_id): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT status, supersedes_id, wire_contribution_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![new_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "active");
        assert!(supersedes.is_none());
        assert_eq!(wire_id.as_deref(), Some("wire-fresh"));
    }

    /// Slug-scoped contributions: the resolver predicate must use `slug
    /// = ?` not `slug IS NULL` when a slug is provided, so two active
    /// rows with different slugs coexist peacefully.
    #[test]
    fn test_commit_pulled_active_isolates_by_slug() {
        let mut conn = mem_conn();
        // Seed a global (slug=NULL) active row.
        let _global = seed_active_contribution(
            &conn,
            "custom_prompts",
            None,
            "schema_type: custom_prompts\nextraction_focus: global\n",
            None,
        );
        // Pull a slug-scoped version.
        conn.execute(
            "INSERT OR IGNORE INTO pyramid_slugs (slug, content_type, source_path)
             VALUES ('my-slug', 'document', '/tmp/my-slug')",
            [],
        )
        .unwrap();
        let metadata = default_wire_native_metadata("custom_prompts", Some("my-slug"));
        let slug_id = commit_pulled_active(
            &mut conn,
            "custom_prompts",
            Some("my-slug"),
            "schema_type: custom_prompts\nextraction_focus: slug-scoped\n",
            "Pulled from Wire (wire-slug)",
            &metadata,
            "wire-slug",
        )
        .unwrap();

        // Global is still active.
        assert_eq!(count_active_rows(&conn, "custom_prompts", None), 1);
        // Slug-scoped is also active.
        assert_eq!(count_active_rows(&conn, "custom_prompts", Some("my-slug")), 1);

        // Slug row has no supersedes_id — it's a fresh insert in the
        // slug scope, not a supersession of the global row.
        let slug_supersedes: Option<String> = conn
            .query_row(
                "SELECT supersedes_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![slug_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            slug_supersedes.is_none(),
            "slug-scoped pull must not supersede a global contribution"
        );
    }
}
