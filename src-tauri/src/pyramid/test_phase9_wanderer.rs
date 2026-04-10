// Phase 9 wanderer tests — verify bugs caught by tracing end-to-end.

#[cfg(test)]
mod wanderer_tests {
    use crate::pyramid::config_contributions::{
        load_active_config_contribution, load_config_version_history, load_contribution_by_id,
    };
    use crate::pyramid::db::init_pyramid_db;
    use crate::pyramid::generative_config::*;
    use crate::pyramid::schema_registry::SchemaRegistry;
    use crate::pyramid::wire_migration::walk_bundled_contributions_manifest;
    use rusqlite::Connection;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    // Regression: accepting a direct YAML when a prior active exists
    // should supersede the prior. The bug: the direct-YAML path calls
    // `create_config_contribution_with_metadata` with status='active'
    // but does NOT touch the prior row, so the DB ends up with two
    // active contributions for the same (schema_type, slug).
    #[test]
    fn wanderer_accept_direct_yaml_does_not_orphan_prior_active() {
        use crate::pyramid::event_bus::BuildEventBus;
        use std::sync::Arc;

        let mut conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = Arc::new(SchemaRegistry::hydrate_from_contributions(&conn).unwrap());
        let bus = Arc::new(BuildEventBus::new());

        // Bundled default evidence_policy is active.
        let bundled = load_active_config_contribution(&conn, "evidence_policy", None)
            .unwrap()
            .unwrap();
        assert_eq!(bundled.status, "active");

        // Now accept a direct-YAML payload (as a user might from the UI
        // editor) — this should supersede the bundled default.
        let yaml = serde_json::Value::String(
            "schema_type: evidence_policy\ntriage_rules: []\ndemand_signals: []\nbudget:\n  max_concurrent_evidence: 4\n".to_string(),
        );
        let resp = accept_config_draft(
            &mut conn,
            &bus,
            &registry,
            "evidence_policy".to_string(),
            None,
            Some(yaml),
            Some("direct accept".to_string()),
        )
        .unwrap();

        // Count active evidence_policy contributions WITH no slug. If
        // the bug is present, there will be TWO active rows.
        let active_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'evidence_policy'
                   AND slug IS NULL
                   AND status = 'active'
                   AND superseded_by_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();

        println!("Active evidence_policy count after direct-YAML accept: {}", active_count);
        println!("Response contribution_id: {}", resp.contribution_id);
        println!("Response version: {}", resp.version);

        // The bundled row should be superseded (or at least no longer
        // shown as the sole active). Check what happened to the bundled row.
        let bundled_after = load_contribution_by_id(&conn, &bundled.contribution_id)
            .unwrap()
            .unwrap();
        println!("Bundled status after accept: {}", bundled_after.status);
        println!(
            "Bundled superseded_by_id after accept: {:?}",
            bundled_after.superseded_by_id
        );

        assert_eq!(
            active_count, 1,
            "Only one active evidence_policy row should exist after accept — otherwise we have orphaned duplicates"
        );

        // Also verify the bundled row is explicitly superseded by the
        // new contribution — this is the contract the spec expects.
        assert_eq!(
            bundled_after.status, "superseded",
            "Bundled default should be marked 'superseded' after a user accept"
        );
        assert_eq!(
            bundled_after.superseded_by_id.as_deref(),
            Some(resp.contribution_id.as_str()),
            "Bundled default's superseded_by_id should point at the new contribution"
        );
    }

    // Regression: refining an ACTIVE contribution must NOT flip the
    // prior row's status to 'superseded' during the draft window.
    // The prior MUST remain active so DADBEAR, builds, and
    // `pyramid_active_config` keep seeing the current policy until
    // the user explicitly accepts the refined draft. Also checks the
    // version number returned to the UI reflects the refinement depth.
    #[test]
    fn wanderer_refine_active_returns_correct_version() {
        let mut conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();

        // Bundled default is already active.
        let active = load_active_config_contribution(&conn, "evidence_policy", None)
            .unwrap()
            .unwrap();
        assert_eq!(active.status, "active");

        // Manually call the same logic the refine IPC would call:
        // 1. Load refinement inputs (this succeeds because active is valid).
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        let inputs = load_refinement_inputs(
            &conn,
            &registry,
            &active.contribution_id,
            &active.yaml_content,
            "make it more conservative",
        )
        .unwrap();

        // 2. Simulate LLM call by faking an output.
        let fake_output =
            "schema_type: evidence_policy\nbudget:\n  max_concurrent_evidence: 1\n";

        // 3. Persist refined draft.
        let resp = persist_refined_draft(&mut conn, &inputs, fake_output).unwrap();

        // The new row is a draft pointing at the prior.
        let new_row = load_contribution_by_id(&conn, &resp.new_contribution_id)
            .unwrap()
            .unwrap();
        assert_eq!(new_row.status, "draft");
        assert_eq!(
            new_row.supersedes_id.as_deref(),
            Some(active.contribution_id.as_str())
        );

        // Wanderer fix: the prior row MUST remain ACTIVE. The refine
        // path creates a draft; acceptance is a separate step. During
        // the draft window the bundled default is still the operative
        // policy.
        let prior_row = load_contribution_by_id(&conn, &active.contribution_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            prior_row.status, "active",
            "prior row must remain active during refine draft window"
        );
        assert_eq!(
            prior_row.superseded_by_id, None,
            "prior row's superseded_by_id must not be set by refine"
        );

        // The active-config lookup must still return the bundled
        // default so DADBEAR / builds keep a valid reference.
        let current_active =
            load_active_config_contribution(&conn, "evidence_policy", None).unwrap();
        assert!(
            current_active.is_some(),
            "refine must not leave evidence_policy without an active row"
        );
        assert_eq!(
            current_active.as_ref().unwrap().contribution_id,
            active.contribution_id,
            "active row must still be the bundled default after refine"
        );

        // Version: the refinement is v2 (bundled default v1 + refine v2).
        assert_eq!(
            resp.version, 2,
            "refine of an active contribution should return version = 2"
        );
    }

    // Sanity: accepting a direct YAML when NO prior active exists
    // works cleanly. Covers the case where a user configures a
    // schema_type with no bundled default (e.g. a per-slug config on
    // a brand new pyramid).
    #[test]
    fn wanderer_accept_direct_yaml_no_prior_active() {
        use crate::pyramid::event_bus::BuildEventBus;
        use std::sync::Arc;

        let mut conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = Arc::new(SchemaRegistry::hydrate_from_contributions(&conn).unwrap());
        let bus = Arc::new(BuildEventBus::new());

        // Use a slug that has no bundled default.
        let yaml = serde_json::Value::String(
            "schema_type: evidence_policy\ntriage_rules: []\ndemand_signals: []\nbudget: {}\n".to_string(),
        );
        let resp = accept_config_draft(
            &mut conn,
            &bus,
            &registry,
            "evidence_policy".to_string(),
            Some("fresh-pyramid".to_string()),
            Some(yaml),
            Some("first config".to_string()),
        )
        .unwrap();

        assert_eq!(resp.status, "active");
        assert_eq!(resp.version, 1, "first accept should be v1");

        let new_row = load_contribution_by_id(&conn, &resp.contribution_id)
            .unwrap()
            .unwrap();
        assert_eq!(new_row.status, "active");
        assert_eq!(new_row.supersedes_id, None, "no prior to supersede");
        assert_eq!(new_row.superseded_by_id, None);
    }

    // Regression: two successive refine calls should produce v2 then v3.
    // Multi-level refinement must correctly count chain depth.
    #[test]
    fn wanderer_multi_refine_increments_version() {
        let mut conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();

        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        let active = load_active_config_contribution(&conn, "evidence_policy", None)
            .unwrap()
            .unwrap();

        // First refine: bundled -> draft_v2
        let inputs_1 = load_refinement_inputs(
            &conn,
            &registry,
            &active.contribution_id,
            &active.yaml_content,
            "more conservative",
        )
        .unwrap();
        let resp_1 = persist_refined_draft(
            &mut conn,
            &inputs_1,
            "schema_type: evidence_policy\nbudget:\n  max_concurrent_evidence: 1\n",
        )
        .unwrap();
        assert_eq!(resp_1.version, 2, "first refine should be v2");

        // Second refine: draft_v2 -> draft_v3 (pointing at the draft from step 1)
        let draft_v2 = load_contribution_by_id(&conn, &resp_1.new_contribution_id)
            .unwrap()
            .unwrap();
        let inputs_2 = load_refinement_inputs(
            &conn,
            &registry,
            &draft_v2.contribution_id,
            &draft_v2.yaml_content,
            "add a rule",
        )
        .unwrap();
        let resp_2 = persist_refined_draft(
            &mut conn,
            &inputs_2,
            "schema_type: evidence_policy\nbudget:\n  max_concurrent_evidence: 2\n",
        )
        .unwrap();
        assert_eq!(resp_2.version, 3, "second refine should be v3");

        // The bundled default remains the only active row throughout.
        let active_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'evidence_policy'
                   AND slug IS NULL
                   AND status = 'active'
                   AND superseded_by_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active_count, 1,
            "exactly one active row should exist through the whole multi-refine chain"
        );
    }
}
