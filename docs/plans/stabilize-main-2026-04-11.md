# Stabilize Main — 2026-04-11

**Author:** Claude (session with Adam)
**Branch:** `stabilize-main` (off `310e34f`)
**Checkpoint:** `stabilize-main-checkpoint-20260411-124944` (commit `a6eb1ae`) — rollback point.
**Status:** Post-Cycle-3 Stage 1 rewrite. 7-commit structure with Cycle 3 critical corrections applied. Ready for Cycle 3 Stage 2.

---

## Cycle history

- **Cycles 1-2:** identified and fixed the obvious bugs (credentials, DADBEAR, Settings UI). Cycle 2 Stage 2 surfaced the bombshell that `pyramid_tier_routing` is effectively decorative — chain execution hardcodes tier mapping and never consults the table.
- **Adam pivot:** "fix it properly, no debt." Option 2 locked in. Modal UX deferred.
- **Cycle 3 Stage 1 (both auditors):** converged on five critical findings that broke the Option 2 plan as first drafted:
  1. **`use_ir_executor: false` on Adam's config** — he's on the legacy chain engine path (`resolve_model`), not the IR path (`resolve_ir_model`). Wiring only `resolve_ir_model` is a no-op for his actual runtime. `dispatch_with_retry` appears 29 times in `chain_executor.rs`, the legacy path is dominant.
  2. **`sync_config_to_operational_with_registry` takes `Option<&Arc<SchemaRegistry>>`, not `Option<&Arc<ProviderRegistry>>`.** Plan's cache-invalidation pseudocode won't compile.
  3. **All 5 IPC sites (main.rs:8133, 8178, 8281, 8370, 8877) call the LEGACY `sync_config_to_operational`**, which internally passes `None` to the registry variant. "No site-specific changes needed" claim was false.
  4. **`walk_bundled_contributions_manifest` runs at main.rs:9763 — BEFORE CredentialStore (9795), ProviderRegistry (9824), BuildEventBus (9905)**. Plan's proposed signature change (thread `bus` + `registry` into the walk) is impossible at the existing call site.
  5. **Legacy `resolve_model` at `chain_dispatch.rs:186` is called from `chain_dispatch.rs:241` inside `dispatch_llm`**, which is used by `run_chain_build`. Plan only wired `resolve_ir_model`. Class fix is incomplete for the dominant path.
- **Plus 10+ major corrections:** `model_aliases` silently dropped; context-limit resolvers not wired; `upsert_tier_routing_from_contribution` signature/return-type mismatch; `&mut Connection` ripple avoidable via `unchecked_transaction()`; `dadbear_policy` is a global no-op (5 schema types, not 6); `IngestSkipped` goes in `event_bus.rs` not `types.rs`; `chain_engine.rs:379-382` has a `VALID_MODEL_TIERS` list to update; Bug #D reseed needs a signature check + explicit DELETE of stale_local row; local_mode.rs has 3 dead `load_from_db` calls post-fix; `chains/CHAIN-DEVELOPER-GUIDE.md` still documents legacy tier names; verification item #8 false-passes because Adam's primary_model coincidentally matches the seeded tier value.

**All critical + major corrections applied below.**

---

## Purpose

Unblock folder ingestion AND fix the architectural class of tier-routing bugs Cycle 2 surfaced. Stop creating debt. Make `pyramid_tier_routing` actually control chain execution **on both the IR path AND the legacy chain path**. Make bundled contributions actually reach operational tables (on a two-pass walk pattern that respects boot ordering). Make cache invalidation actually work (by adding a `ProviderRegistry` parameter to the sync dispatcher, not by conflating it with `SchemaRegistry`).

Not the Self-Describing Filesystem pivot. Not the tier routing Settings modal. Not the generic credentials manager.

---

## Core Architectural Facts (verified post-Cycle-3)

Before the bug list, the load-bearing facts that shape the plan:

1. **Adam's boot flags:** `use_ir_executor: false, use_chain_engine: true`. Verified by reading `~/Library/Application Support/wire-node/pyramid_config.json`. Production builds dispatch via `run_chain_build` → `execute_chain_from` → `dispatch_step` → `dispatch_llm` → `resolve_model` at `chain_dispatch.rs:186`. The IR path (`run_ir_build` → `resolve_ir_model`) is present but unused on Adam's runtime. **Both resolvers must be wired for the class fix to reach Adam's actual builds.**

2. **Two distinct registries:** `ProviderRegistry` (Phase 3, `provider.rs`) holds tier routing + credential resolution. `SchemaRegistry` (Phase 9, `schema_registry.rs`) holds schema definitions. `sync_config_to_operational_with_registry` currently takes `Option<&Arc<SchemaRegistry>>` used by the `schema_definition` branch. Plan needs to ADD a new `provider_registry: Option<&Arc<ProviderRegistry>>` parameter; they are not interchangeable.

3. **Boot order:** `init_pyramid_db(writer)` at main.rs:9688 → `ensure_default_chains` at 9743 → `migrate_prompts_and_chains_to_contributions` (contains `walk_bundled_contributions_manifest`) at 9763 → `CredentialStore::load` at 9795 → `ProviderRegistry::new + load_from_db` at 9824-9832 → `BuildEventBus::new` inside `PyramidState` at ~9905. The walk cannot take `bus` or `registry` as parameters at the existing call site — they don't exist yet. Two-pass walk is required: Pass 1 = insert at current location (unchanged); Pass 2 = sync after bus + registry creation.

4. **`chain_dispatch::resolve_ir_model` at line 1023** is the IR resolver, called from 7 sites in `chain_executor.rs` (11185, 11205, 11382, 11664, 11687, 12070, 12115). **`chain_dispatch::resolve_model` at line 186** is the LEGACY resolver, called from `dispatch_llm` at `:241`, which feeds `dispatch_step` / `dispatch_with_retry` used throughout the `run_chain_build` path. Both need fixing. Both currently hardcode `low|mid|high|max` + `model_aliases` lookup + `primary_model` fallback. Neither consults `pyramid_tier_routing`.

5. **`call_model_via_registry` at `llm.rs:1805` has zero production callers.** It's the only function that currently reads the tier routing table for actual LLM dispatch. Evidence that the table was never wired: the function exists but is never called outside its own module.

6. **`provider::resolve_tier` is called from `generative_config.rs:233`, `migration_config.rs:567`, and test sites in `provider.rs`.** In `generative_config`/`migration_config`, the resolved model is used only for cache-key metadata on `StepContext`, not for actual LLM dispatch. The dispatch still uses `config.primary_model` via `call_model_unified_with_options_and_ctx`.

7. **`walk_bundled_contributions_manifest` at `wire_migration.rs:1044-1089` does `INSERT OR IGNORE` into `pyramid_config_contributions` but never calls any sync function.** Six bundled schema types (`build_strategy`, `dadbear_policy`, `evidence_policy`, `folder_ingestion_heuristics`, `tier_routing`, `custom_prompts`) never reach operational tables on fresh installs. **Note:** `dadbear_policy` is a no-op for global (slug=None) per `db.rs:14335-14347`, so the effective count is 5 schema types.

8. **`seed_default_provider_registry` (Rust at `db.rs:12908-12996`) seeds 4 tiers** (`fast_extract`, `web`, `synth_heavy`, `stale_remote`). Doc comment explicitly says: *"`stale_local` is NOT seeded — only exists once a user registers a local provider. Do not insert a row pointing at a placeholder; the absence is deliberate per Adam's decision."* The **bundled `tier_routing` contribution at `bundled_contributions.json:108-114` has 3 tiers** (`fast_extract`, `synth_heavy`, `stale_local`) pointing at cloud — directly contradicting the Rust doc comment. Adam's live DB proves the bundled version won via a `local_mode_toggle` supersession at 15:31:48 today: 3 rows, missing `web` and `stale_remote`, with the lying `stale_local` row present.

9. **Chain YAMLs reference `model_tier: extractor`** in `chains/defaults/conversation*.yaml` and `question.yaml`. Nothing in Rust code, seed, or bundled contribution defines `extractor`. Every step using it silently falls through to `primary_model`. Works for Adam by luck because `primary_model = inception/mercury-2` is a reasonable extraction model.

10. **`upsert_tier_routing_from_contribution` at `db.rs:14445-14516` is destructive** — DELETEs any tier not in the incoming contribution, then INSERTs/UPDATEs the listed ones. Current signature: `(conn: &Connection, yaml: &TierRoutingYaml, _contribution_id: &str) -> Result<()>`. The function synthesizes `pricing_json` from `prompt_price_per_token` + `completion_price_per_token` YAML fields at lines 14483-14502 — this synthesis must be preserved in the additive rewrite.

11. **`save_tier_routing` at `db.rs:12742-12773` already does `ON CONFLICT DO UPDATE` upserts.** The additive rewrite of `upsert_tier_routing_from_contribution` should reuse this helper inside a loop, not duplicate the SQL.

12. **`&Connection` with `unchecked_transaction()` is idiomatic** in this codebase — used already in `delta.rs:340, 830`, `stale_engine.rs:1430`, `db.rs:2449, 2512`. The plan should use this to avoid rippling `&mut Connection` through 20+ call sites.

13. **`folder_ingestion.rs` emits `RegisterDadbearConfig` at FOUR sites:** lines 985 (leaf pyramid), 1058 (grouped vine), 1202 (CC conversation bedrock — Conversation, stays unchanged), 1258 (CC memory bedrock — Document, must be filtered).

14. **`DadbearWatchConfig.content_type` is `String`, not `ContentType` enum.** Any hoist check must use `.as_str()` string comparison, not `matches!` against the enum.

15. **Tests `test_fire_ingest_chain_code_scope_error` at `dadbear_extend.rs:1354` and `test_fire_ingest_chain_document_scope_error` at `:1388`** codify the Phase 0b broken behavior. Must be deleted in the same commit that removes the arm.

16. **`TaggedKind` is defined in `event_bus.rs:37`, NOT `types.rs`.** `IngestSkipped` variant addition goes in event_bus.rs.

17. **`chain_engine.rs:379-382` has a `VALID_MODEL_TIERS` const** = `&["low", "mid", "high", "max", "extractor", "synth_heavy", "web"]` used by `validate_chain` for warnings. Missing `fast_extract`, `stale_local`, `stale_remote`. The `extractor → fast_extract` rename + class fix must update this list.

18. **`chains/CHAIN-DEVELOPER-GUIDE.md`** at lines 40, 98, 274 documents `low|mid|high|max` as the canonical tier names. Out of date but load-bearing for new chain authors.

19. **Live DB state on Adam's machine:**
    - 65 zero-node slugs from today's failed ingest.
    - 1088 failed records: 6 credential errors, 1082 Phase 0b errors.
    - 37 code/document dadbear_configs.
    - 3 rows in `pyramid_tier_routing`: `fast_extract`, `stale_local`, `synth_heavy`, all → `openrouter|inception/mercury-2`.
    - `.credentials` does NOT exist.
    - `pyramid_config.json` has `openrouter_api_key = sk-or-v1-ae7abf...` (73 chars real).

---

## The Bugs + Architectural Class Fix In Scope

### Bug #1 — `.credentials` file never bootstraps from legacy config (P0)

**Error origin:** `credentials.rs:316-328` (`resolve_var`) reached via `provider.rs:958` (bare-variable branch of `resolve_credential_for`).

**Fix:** New `CredentialStore::load_with_bootstrap(path, data_dir) -> Result<(Arc<Self>, BootstrapReport)>` API.

**Serde struct for bootstrap read:** Use a minimal struct, NOT `PyramidConfig`:

```rust
// credentials.rs — private helper struct
#[derive(serde::Deserialize)]
struct BootstrapLegacyKey {
    #[serde(default)]
    openrouter_api_key: String,
}
```

This is resilient to schema drift (unknown fields ignored), fails cleanly on missing `openrouter_api_key`, and doesn't depend on the full `PyramidConfig` struct.

```rust
fn read_legacy_openrouter_key(data_dir: &Path) -> Result<Option<String>> {
    let config_path = data_dir.join("pyramid_config.json");
    let contents = match fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            tracing::debug!("no legacy pyramid_config.json to bootstrap from");
            return Ok(None);
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to read legacy pyramid_config.json");
            return Ok(None);
        }
    };
    let parsed: BootstrapLegacyKey = match serde_json::from_str(&contents) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "legacy pyramid_config.json is malformed — skipping credential bootstrap. \
                 Fix via Settings or delete the file to retry."
            );
            return Ok(None);
        }
    };
    let legacy = parsed.openrouter_api_key.as_str();
    if legacy.is_empty()
        || legacy.trim() != legacy
        || legacy.starts_with("${")
        || legacy.starts_with('"')
        || legacy.len() < 20
    {
        return Ok(None);
    }
    Ok(Some(legacy.to_string()))
}
```

Path guard: bootstrap only fires when `path.ends_with(".credentials")` (not `.credentials.fallback`).

**Main.rs wiring (exact site):** Replace `CredentialStore::load(...)` at `main.rs:9795` with `load_with_bootstrap`:

```rust
let (credential_store, bootstrap_report) = match
    CredentialStore::load_with_bootstrap(
        config.data_dir().join(".credentials"),
        config.data_dir(),
    )
{
    Ok((store, report)) => (store, report),
    Err(e) => {
        tracing::error!(error = %e, "credential store load_with_bootstrap failed");
        // Fallback at main.rs:9806 and :9813 stays as-is — no bootstrap via fallback
        // path. Documented limitation (Bug #26). If the user hits this, they see an
        // empty store until they restart with a readable .credentials.
        (Arc::new(...existing fallback...), BootstrapReport::default())
    }
};

if bootstrap_report.bootstrapped {
    match db::open_pyramid_connection(&pyramid_db_path) {
        Ok(conn) => {
            match db::retry_credential_failed_ingest_records(&conn) {
                Ok(count) => tracing::info!(count, "Reset credential-failed ingest records"),
                Err(e) => tracing::warn!(error = %e, "credential-failed retry failed"),
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to open DB for post-bootstrap retry"),
    }
}
```

**Retry helper:**

```rust
// db.rs
pub fn retry_credential_failed_ingest_records(conn: &Connection) -> Result<usize> {
    let affected = conn.execute(
        "UPDATE pyramid_ingest_records
         SET status='pending', error_message=NULL, updated_at=datetime('now')
         WHERE status='failed'
           AND (error_message LIKE '%config references credential%'
                OR error_message LIKE '%OPENROUTER_KEY%')",
        [],
    )?;
    Ok(affected)
}
```

Credential-scoped only (6 rows in Adam's DB). The 1082 Phase 0b records get deleted by Commit 6's historical cleanup migration.

**Unit tests:** 11, unchanged from prior rev.

---

### Bug #2 — DADBEAR Code/Document four-part fix (P1)

**Part A — Hoisted skip at TWO layers:**

**Pseudocode corrected** (Cycle 3 Stage 1 A Issue 17 — real variable names):

At the TOP of `dispatch_pending_ingests` (before the DB-opening fetch block):

```rust
async fn dispatch_pending_ingests(
    db_path: &str,
    config: &DadbearWatchConfig,
    bus: &Arc<BuildEventBus>,
) -> Result<()> {
    // Hoisted skip: Code/Document slugs are built by folder_ingestion's
    // first-build dispatch via question_build::spawn_question_build.
    // DADBEAR has no job here.
    if matches!(config.content_type.as_str(), "code" | "document") {
        let conn = db::open_pyramid_connection(Path::new(db_path))?;
        // Fetch any currently-pending records and mark them skipped.
        let pending: Vec<IngestRecord> = db::get_pending_ingests(&conn, &config.slug)?;
        for record in &pending {
            if let Err(e) = db::mark_ingest_skipped(
                &conn,
                record.id,
                "handled by folder_ingestion first-build dispatch",
            ) {
                tracing::warn!(record_id = record.id, error = %e, "mark_ingest_skipped failed");
            }
        }
        tracing::info!(
            slug = %config.slug,
            content_type = %config.content_type,
            record_count = pending.len(),
            "DADBEAR skipping Code/Document ingest (folder_ingestion-managed)"
        );
        return Ok(());
    }
    // ... existing claim + dispatch logic ...
}
```

At the TOP of `run_tick_for_config`:

```rust
async fn run_tick_for_config(
    db_path: &str,
    config: &DadbearWatchConfig,
    bus: &Arc<BuildEventBus>,
) -> Result<()> {
    // Early return: Code/Document never needs DADBEAR tick-based scanning.
    if matches!(config.content_type.as_str(), "code" | "document") {
        return Ok(());
    }
    // ... existing scan + detect_changes + dispatch logic ...
}
```

**`mark_ingest_skipped` helper in db.rs:**

```rust
pub fn mark_ingest_skipped(conn: &Connection, record_id: i64, reason: &str) -> Result<()> {
    let affected = conn.execute(
        "UPDATE pyramid_ingest_records
         SET status='skipped', error_message=?2, updated_at=datetime('now')
         WHERE id=?1 AND status='pending'",
        params![record_id, reason],
    )?;
    if affected == 0 {
        tracing::warn!(record_id, "mark_ingest_skipped: record not pending (raced)");
    }
    Ok(())
}
```

Conditional UPDATE guards TOCTOU. `'skipped'` is schema-legal (no CHECK constraint on `status`).

**Part B — Delete codifying tests:**

- Delete `test_fire_ingest_chain_code_scope_error` at `dadbear_extend.rs:1354`.
- Delete `test_fire_ingest_chain_document_scope_error` at `dadbear_extend.rs:1388`.
- Add new tests: `test_dispatch_pending_ingests_skips_code`, `test_dispatch_pending_ingests_skips_document`, `test_run_tick_for_config_early_returns_for_code`, `test_run_tick_for_config_processes_conversation_normally`.
- Delete the `ContentType::Code | ContentType::Document` arm at `fire_ingest_chain:742-748`. It's unreachable after Part A.

**Part C — Four emission sites (corrected from three):**

`folder_ingestion.rs` emits `RegisterDadbearConfig` at:
- **Line 985** — leaf pyramid (Code/Document possible)
- **Line 1058** — grouped vine (Code/Document possible)
- **Line 1202** — CC conversation bedrock (Conversation, stays unchanged)
- **Line 1258** — CC memory bedrock (Document, MUST be filtered — Cycle 2 Stage 1 A Issue 2)

Refactor through a helper with doc comment per Cycle 3 Stage 1 A Issue 16:

```rust
// folder_ingestion.rs — helper
//
// Allow-list semantics: only Conversation content types get DADBEAR configs.
// Code/Document slugs are handled by folder_ingestion's first-build dispatch.
// If a new non-file-ingest ContentType variant is added in the future, it
// will silently skip here — which is the safe default. If that variant should
// emit a DADBEAR config, update this helper.
fn maybe_emit_dadbear_config(
    ops: &mut Vec<IngestionOperation>,
    slug: String,
    source_path: String,
    content_type: &str,
    scan_interval_secs: u64,
) {
    // Allow-list: only Conversation emits.
    if content_type != "conversation" {
        return;
    }
    ops.push(IngestionOperation::RegisterDadbearConfig {
        slug,
        source_path,
        content_type: content_type.to_string(),
        scan_interval_secs,
    });
}
```

Update lines 985, 1058, 1202, 1258 to call this helper. Line 1202 (Conversation) continues to emit; the other three skip.

Add unit test: `execute_plan` on a mixed-content folder produces `RegisterDadbearConfig` ops ONLY for Conversation-typed slugs.

**Part D — `IngestSkipped` event variant + historical cleanup migration:**

`IngestSkipped` variant goes in **`event_bus.rs` at `TaggedKind`** (not `types.rs` — Cycle 3 Stage 1 B Issue 7):

```rust
// event_bus.rs near line 37
pub enum TaggedKind {
    IngestScanComplete { ... },
    IngestStarted { ... },
    IngestComplete { ... },
    IngestFailed { ... },
    IngestSkipped { source_path: String, reason: String },  // NEW
    // ... other variants ...
}
```

**Historical cleanup migration** (one-shot, gated by `_migration_marker` sentinel with `created_by='stabilize_main_dadbear_cleanup_v1'`, following the pattern at `db.rs:1960-2070`):

```sql
-- Runs once. After emission-site fix, no new rows match on subsequent boots.
DELETE FROM pyramid_dadbear_config WHERE content_type IN ('code', 'document');
-- 37 rows expected in Adam's DB.

DELETE FROM pyramid_ingest_records
WHERE status='failed' AND error_message LIKE '%Phase 0b%';
-- 1082 rows expected in Adam's DB.
```

---

### Bug #4 — Settings UI credential wire (P0)

Unchanged from prior rev. Three sites: `PyramidSettings.tsx:42-61` (handleSave), `:63-85` (handleTestApiKey), `PyramidFirstRun.tsx:27-36` (handleSaveApiKey). Add parallel `pyramid_set_credential` call, use `apiKey.trim()`, add `autoExecute` to handleSave dep array, add `credentialWriteFailed` state for partial-success UX.

---

### Bug #25 — `walk_bundled_contributions_manifest` two-pass pattern (P1, ARCHITECTURAL)

**Corrected approach per Cycle 3 Stage 1 (both auditors):**

The walk runs at `main.rs:9763` BEFORE CredentialStore (9795), ProviderRegistry (9824), and BuildEventBus (9905) exist. Cannot thread them through. **Use a two-pass pattern:**

**Pass 1 — unchanged:** `walk_bundled_contributions_manifest(conn)` keeps its current signature. It does `INSERT OR IGNORE` into `pyramid_config_contributions` and returns a `BundledMigrationReport` with a new `newly_inserted: Vec<String>` field listing the `contribution_id`s that were newly inserted (vs already-present).

**Pass 2 — new function:** `sync_bundled_contributions_to_operational(conn, bus, schema_registry, provider_registry, newly_inserted)` runs AFTER `PyramidState` is constructed (around main.rs:9910, where bus and both registries exist). For each `contribution_id`, it:
1. Loads the contribution row from `pyramid_config_contributions`.
2. Calls `sync_config_to_operational_with_registry(conn, bus, &contribution, schema_registry, provider_registry)` (the new 5-arg signature from Commit 3).
3. Wraps each (load + sync) pair in a transaction via `conn.unchecked_transaction()?`. On sync failure, rollback the transaction AND delete the contribution row (so it'll be re-inserted + re-attempted on next boot).
4. WARN-logs failures but continues processing remaining IDs.
5. Returns a report with succeeded/failed counts.

**Why rollback on failure (not WARN-and-continue):** Cycle 3 Stage 1 A Issue 10 — if we leave the contribution row active when sync fails, we recreate Bug #25's state (contribution row present, operational table stale). Rolling back the row makes the next boot retry cleanly.

**Boot wiring in main.rs:**

```rust
// main.rs around line 9763 (existing location, unchanged signature)
let bundled_report = match wire_node_lib::pyramid::wire_migration::migrate_prompts_and_chains_to_contributions(
    &pyramid_writer,
    ...
) {
    Ok(r) => r,
    Err(e) => { /* existing error handling */ }
};
// `bundled_report.newly_inserted` is now a Vec<String> of contribution IDs.

// ... existing boot steps: credential_store load, provider_registry new + hydrate, schema_registry ...

// NEW — after PyramidState is constructed (bus + both registries exist):
if !bundled_report.newly_inserted.is_empty() {
    match wire_node_lib::pyramid::wire_migration::sync_bundled_contributions_to_operational(
        &pyramid_writer,
        &pyramid_state.build_event_bus,
        Some(&pyramid_state.schema_registry),
        Some(&pyramid_state.provider_registry),
        &bundled_report.newly_inserted,
    ) {
        Ok(report) => tracing::info!(
            succeeded = report.succeeded,
            failed = report.failed,
            "bundled contributions post-boot sync complete"
        ),
        Err(e) => tracing::error!(error = %e, "bundled contributions post-boot sync failed"),
    }
}
```

**Scope note — `dadbear_policy` is NOT a schema type this fixes:** Per Cycle 3 Stage 1 A Issue 7, `upsert_dadbear_policy` at `db.rs:14335-14347` early-returns for `slug=None` (global). Bundled `dadbear_policy` ships as global, so after the fix the sync dispatcher will no-op on it. **5 schema types actually fixed** by Bug #25: `build_strategy`, `evidence_policy`, `folder_ingestion_heuristics`, `tier_routing` (deleted per Bug #D below), `custom_prompts`. The "6 schema types" claim from prior revs overcounted.

**`newly_inserted` tracking:** `walk_bundled_contributions_manifest` already returns a `BundledMigrationReport`. Extend it with a `newly_inserted: Vec<String>` field. `insert_bundled_contribution` at `wire_migration.rs:978` already returns a success/exists discriminator — use that to populate the vec.

**Unit tests:**
- `test_walk_bundled_reports_newly_inserted` — walks a manifest on an empty DB, asserts all IDs are in `newly_inserted`.
- `test_walk_bundled_skips_existing_on_rerun` — walks twice, asserts second run has empty `newly_inserted`.
- `test_sync_bundled_post_boot_populates_operational` — runs pass 1 + pass 2, asserts 5 operational tables are populated.
- `test_sync_bundled_post_boot_rolls_back_on_failure` — simulates a sync failure on one entry, asserts that contribution row is deleted and the others succeed.

---

### Bug #D — Seed vs bundled tier list reconciliation + stale_local lie removal (P1, DATA RECONCILIATION)

**Changes:**

**(a) Delete the `tier_routing` block from `bundled_contributions.json`.** Rust seed becomes the canonical source.

**(b) One-shot reseed migration for Adam's specific broken state, gated by `_migration_marker` sentinel with `created_by='stabilize_main_tier_reseed_v1'`:**

Per Cycle 3 Stage 1 B Issue 9 and A Issue 19: add a **Rust-side signature check** before running the SQL. Only fire the migration on DBs that match Adam's specific broken state, to avoid clobbering users with custom tier layouts.

```rust
// db.rs — new migration runs once per installed version
fn maybe_reseed_tiers_for_bundled_lie(conn: &Connection) -> Result<()> {
    // Signature check: Adam's broken state is exactly:
    // - 3 rows in pyramid_tier_routing
    // - 'stale_local' present (the bundled lie)
    // - 'web' absent
    // - 'stale_remote' absent
    let row_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_tier_routing",
        [],
        |row| row.get(0),
    )?;
    if row_count != 3 {
        tracing::debug!(row_count, "skipping tier reseed — not the known-broken signature");
        return Ok(());
    }
    let has_stale_local: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pyramid_tier_routing WHERE tier_name='stale_local')",
        [],
        |row| row.get(0),
    )?;
    let has_web: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pyramid_tier_routing WHERE tier_name='web')",
        [],
        |row| row.get(0),
    )?;
    let has_stale_remote: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pyramid_tier_routing WHERE tier_name='stale_remote')",
        [],
        |row| row.get(0),
    )?;
    if !has_stale_local || has_web || has_stale_remote {
        tracing::debug!("skipping tier reseed — not the known-broken signature");
        return Ok(());
    }

    // Matches the known-broken signature. Reseed the missing tiers AND
    // delete the stale_local lie row so local_mode.rs can recreate it
    // correctly if/when local mode is enabled.
    tracing::info!("Detected bundled-lie tier state. Reseeding web + stale_remote + removing stale_local lie row.");

    conn.execute(
        "INSERT OR IGNORE INTO pyramid_tier_routing
         (tier_name, provider_id, model_id, context_limit, max_completion_tokens, pricing_json, notes, created_at, updated_at)
         VALUES
         ('web', 'openrouter', 'x-ai/grok-4.1-fast', 2000000, NULL, '{}', 'Reseeded by stabilize-main: Adam default 2M context.', datetime('now'), datetime('now'))",
        [],
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO pyramid_tier_routing
         (tier_name, provider_id, model_id, context_limit, max_completion_tokens, pricing_json, notes, created_at, updated_at)
         VALUES
         ('stale_remote', 'openrouter', 'minimax/minimax-m2.7', 200000, NULL, '{}', 'Reseeded by stabilize-main: near-frontier for upper-layer stale checks.', datetime('now'), datetime('now'))",
        [],
    )?;

    // Remove the bundled lie: stale_local pointing at cloud OpenRouter.
    // If user later enables local mode, local_mode.rs will recreate it
    // pointing at Ollama. Per Rust doc comment: "stale_local only exists
    // once a user registers a local provider."
    conn.execute(
        "DELETE FROM pyramid_tier_routing
         WHERE tier_name='stale_local' AND provider_id='openrouter'",
        [],
    )?;

    Ok(())
}
```

**Sentinel:** write the `_migration_marker` row regardless of whether the migration body fired. One run per install. If a user's DB is in a different broken state, they get no repair — document in the plan as a known limitation, not a bug.

**(c) Post-reseed expected state for Adam's DB:** `fast_extract`, `synth_heavy`, `web`, `stale_remote` (4 rows; Rust-seeded values for all four, `stale_local` deleted).

---

### Bug #L — Rename `extractor` → `fast_extract` in chain YAMLs + docs + VALID_MODEL_TIERS (P2, CHAIN RECONCILIATION)

**Files:**
- `chains/defaults/conversation-chronological.yaml`
- `chains/defaults/conversation-episodic.yaml`
- `chains/defaults/conversation.yaml`
- `chains/defaults/conversation-episodic-fast.yaml`
- `chains/defaults/question.yaml`
- Any `chains/prompts/**/*.yaml` with `model_tier: extractor`
- **`src-tauri/src/pyramid/chain_engine.rs:379-382`** (Cycle 3 Stage 1 B Issue 8) — update `VALID_MODEL_TIERS`:

```rust
const VALID_MODEL_TIERS: &[&str] = &[
    "low", "mid", "high", "max",               // legacy fallback names
    "fast_extract", "synth_heavy", "web",       // from Rust seed
    "stale_remote", "stale_local",              // from Rust seed + local mode
];
```

- **`chains/CHAIN-DEVELOPER-GUIDE.md`** (Cycle 3 Stage 1 A Issue 9) — lines 40, 98, 274 update to include the 5 canonical tier names plus legacy fallbacks. New chain authors should see the full set.

**Boot-time diagnostic scanner** (NOT auto-populate, per prior decisions): after `seed_default_provider_registry` + the Bug #D reseed migration + `ensure_default_chains`, run a scan that walks `chains/defaults/*.yaml` and `chains/prompts/**/*.yaml` for `model_tier:` values. For each unique tier name NOT in the current `pyramid_tier_routing` table AND NOT in `{low, mid, high, max}` (legacy fallbacks), log a WARN so the ops surface is visible.

**Location for the scanner:** `scan_chain_tiers_and_warn(chains_dir: &Path, conn: &Connection)` called from main.rs between `ensure_default_chains` (line 9742, where `chains_dir` first exists) and `provider_registry.load_from_db` (line 9832). Diagnostic only; no table writes.

---

### Architectural class fix: Wire BOTH resolvers + `resolve_context_limit` (P0)

**Cycle 3 Stage 1 A Issue 1 is the most important finding of this audit cycle.** Adam's `use_ir_executor: false` config means wiring only the IR path is a no-op for his production runtime. BOTH resolvers must be wired.

**Resolver sites to wire:**

1. **`chain_dispatch::resolve_ir_model` at `:1023`** (IR path, 7 call sites in chain_executor.rs)
2. **`chain_dispatch::resolve_model` at `:186`** (legacy path, called from `dispatch_llm` at `:241`, used by `run_chain_build`)
3. **`chain_dispatch::resolve_ir_context_limit` at `:1056-1079`** (IR context limit)
4. **`chain_dispatch::resolve_context_limit` at `:1085-1114`** (legacy context limit)

**Unified new shape (both resolvers, same pattern):**

```rust
pub fn resolve_ir_model(reqs: &ModelRequirements, config: &LlmConfig) -> String {
    let tier_name = match reqs.tier.as_deref() {
        Some(name) if !name.is_empty() => name,
        _ => return config.primary_model.clone(),
    };

    // LAYER 1: model_aliases escape hatch (preserved per Cycle 3 Stage 1 A Issue 6)
    if let Some(model) = config.model_aliases.get(tier_name) {
        return model.clone();
    }

    // LAYER 2: provider_registry table lookup (NEW — the class fix)
    if let Some(registry) = config.provider_registry.as_ref() {
        if let Ok(resolved) = registry.resolve_tier(tier_name, None, None, None) {
            return resolved.tier.model_id.clone();
        }
        // Tier not in table — fall through to legacy mapping.
    }

    // LAYER 3: legacy hardcoded mapping (preserved — low|mid|high|max intentionally aren't in the table)
    match tier_name {
        "low" | "mid" => config.primary_model.clone(),
        "high" => config.fallback_model_1.clone(),
        "max" => config.fallback_model_2.clone(),
        other => {
            tracing::warn!(
                tier = other,
                "[IR] tier not in pyramid_tier_routing, model_aliases, or legacy mapping — falling back to primary_model"
            );
            config.primary_model.clone()
        }
    }
}
```

**Legacy `resolve_model` gets the exact same shape**, adapted to its signature `(step: &ChainStep, defaults: &ChainDefaults, config: &LlmConfig)`:

```rust
fn resolve_model(step: &ChainStep, defaults: &ChainDefaults, config: &LlmConfig) -> String {
    // Direct model override — unchanged, wins first.
    if let Some(model) = step.model_override.as_ref() {
        return model.clone();
    }
    // Tier resolution — same three-layer shape as resolve_ir_model.
    let tier_name = step.model_tier.as_deref()
        .or(defaults.model_tier.as_deref())
        .filter(|t| !t.is_empty());
    let tier_name = match tier_name {
        Some(t) => t,
        None => return config.primary_model.clone(),
    };

    if let Some(model) = config.model_aliases.get(tier_name) {
        return model.clone();
    }
    if let Some(registry) = config.provider_registry.as_ref() {
        if let Ok(resolved) = registry.resolve_tier(tier_name, None, None, None) {
            return resolved.tier.model_id.clone();
        }
    }
    match tier_name {
        "low" | "mid" => config.primary_model.clone(),
        "high" => config.fallback_model_1.clone(),
        "max" => config.fallback_model_2.clone(),
        other => {
            tracing::warn!(tier = other, "[CHAIN] tier not in pyramid_tier_routing — fallback");
            config.primary_model.clone()
        }
    }
}
```

**Context-limit resolvers get the parallel wiring:**

```rust
pub fn resolve_ir_context_limit(reqs: &ModelRequirements, tier1: &Tier1Config, config: &LlmConfig) -> usize {
    let tier_name = match reqs.tier.as_deref() {
        Some(name) if !name.is_empty() => name,
        _ => return config.primary_context_limit,
    };
    // Table first (new)
    if let Some(registry) = config.provider_registry.as_ref() {
        if let Ok(resolved) = registry.resolve_tier(tier_name, None, None, None) {
            if let Some(limit) = resolved.tier.context_limit {
                return limit;
            }
        }
    }
    // Legacy fallback (unchanged)
    match tier_name {
        "high" => tier1.high_tier_context_limit,
        "max" => tier1.max_tier_context_limit,
        _ => config.primary_context_limit,
    }
}
```

Same pattern for `resolve_context_limit`.

**Why it's safe to wire both:**
- The legacy fallback (LAYER 3) preserves pre-fix behavior for tiers NOT in the table.
- `low|mid|high|max` intentionally aren't in the table; they still work.
- `model_aliases` escape hatch is preserved (LAYER 1, currently unused but documented).
- Zero regression risk for Adam's running chains because `primary_model = inception/mercury-2` and the seed tiers all point at the same thing.

**Unit tests (for BOTH resolvers):**
- `test_resolve_{ir,}_model_registry_wins_over_legacy` — registry populated with `fast_extract → custom/model-x`; resolver returns `"custom/model-x"` not `primary_model`.
- `test_resolve_{ir,}_model_model_aliases_wins_over_registry` — both populated; alias wins.
- `test_resolve_{ir,}_model_falls_through_when_tier_not_in_table` — registry empty, `tier = "fast_extract"` → legacy fallback returns `primary_model`.
- `test_resolve_{ir,}_model_legacy_low_mid_high_max` — each maps to the right `primary_model`/`fallback_model_1`/`fallback_model_2` (unchanged behavior).
- `test_resolve_{ir,}_model_empty_tier_falls_back_to_primary` — `tier = Some("")` → `primary_model`.
- `test_resolve_{ir,}_context_limit_reads_from_table` — registry has `fast_extract` with `context_limit=120_000`; resolver returns 120000 not `primary_context_limit`.

---

### Additive upsert with `unchecked_transaction()` (P0)

**Cycle 3 Stage 1 A Issue 5 + B Issue 10:** Drop the `&mut Connection` ripple. Use `conn.unchecked_transaction()?` instead. Reuse `save_tier_routing` helper inside the loop instead of duplicating the SQL.

```rust
// db.rs
pub fn upsert_tier_routing_from_contribution(
    conn: &Connection,
    yaml: &TierRoutingYaml,
    contribution_id: &str,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    for entry in &yaml.entries {
        // Preserve the existing pricing synthesis logic from db.rs:14483-14502:
        // combine prompt_price_per_token + completion_price_per_token into pricing_json
        // if present. Keep the existing helper logic or inline it here.
        let pricing_json = synthesize_pricing_json(
            entry.prompt_price_per_token,
            entry.completion_price_per_token,
            entry.pricing_json.as_deref(),
        );
        let tier_row = TierRoutingEntry {
            tier_name: entry.tier_name.clone(),
            provider_id: entry.provider_id.clone(),
            model_id: entry.model_id.clone(),
            context_limit: entry.context_limit,
            max_completion_tokens: entry.max_completion_tokens,
            pricing_json,
            supported_parameters_json: entry.supported_parameters_json.clone(),
            notes: entry.notes.clone(),
        };
        // save_tier_routing already does INSERT ... ON CONFLICT DO UPDATE.
        save_tier_routing(&tx, &tier_row)?;
    }

    tx.commit()?;
    tracing::info!(
        contribution_id = %contribution_id,
        tier_count = yaml.entries.len(),
        "upsert_tier_routing_from_contribution: merged (additive)"
    );
    Ok(())
}
```

**Key invariants:**
- **Never DELETE rows** not in the incoming set. Contributions only ADD to or MODIFY existing tiers.
- **Transaction atomicity** — if any row fails (e.g., FK violation), rollback.
- **`&Connection` signature unchanged** — no ripple through 20+ call sites.
- **Pricing synthesis logic preserved** — lifted from the existing function body.

Signature stays `(conn: &Connection, yaml: &TierRoutingYaml, contribution_id: &str) -> Result<()>` — same as current. Callers at `config_contributions.rs:678` (and anywhere else) don't need updating.

**Unit tests:**
- `test_upsert_tier_routing_is_additive` — populate table with 5 tiers, sync a contribution with 2, assert 5 tiers remain (2 updated, 3 untouched).
- `test_upsert_tier_routing_rolls_back_on_fk_violation` — populate contribution with a row pointing at a nonexistent provider_id; assert original table state is preserved.
- `test_upsert_tier_routing_preserves_pricing_synthesis` — contribution with `prompt_price_per_token + completion_price_per_token`; assert resulting row has `pricing_json` synthesized correctly.

---

### Cache invalidation via `ProviderRegistry` parameter (P0)

**Cycle 3 Stage 1 A Issue 2 + B Issues 1+2.** The plan's prior pseudocode conflated `SchemaRegistry` and `ProviderRegistry`. Fix: add a NEW parameter to `sync_config_to_operational_with_registry`.

**New signature:**

```rust
// config_contributions.rs
pub fn sync_config_to_operational(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    contribution: &ConfigContribution,
) -> Result<(), ConfigSyncError> {
    sync_config_to_operational_with_registry(conn, bus, contribution, None, None)
}

pub fn sync_config_to_operational_with_registry(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    contribution: &ConfigContribution,
    schema_registry: Option<&Arc<SchemaRegistry>>,
    provider_registry: Option<&Arc<ProviderRegistry>>,  // NEW
) -> Result<(), ConfigSyncError> {
    validate_yaml_against_schema(&contribution.yaml_content, &contribution.schema_type)?;

    match contribution.schema_type.as_str() {
        "tier_routing" => {
            let yaml = parse_tier_routing(&contribution.yaml_content)?;
            db::upsert_tier_routing_from_contribution(conn, &yaml, &contribution.contribution_id)?;
            // Refresh the in-memory provider registry so the new tiers are immediately visible.
            if let Some(reg) = provider_registry {
                reg.load_from_db(conn)
                    .context("refreshing provider_registry after tier_routing sync")?;
            }
        }
        "step_overrides" => {
            let yaml = parse_step_overrides(&contribution.yaml_content)?;
            db::upsert_step_overrides_from_contribution(conn, &yaml, &contribution.contribution_id)?;
            if let Some(reg) = provider_registry {
                reg.load_from_db(conn)
                    .context("refreshing provider_registry after step_overrides sync")?;
            }
        }
        "schema_definition" => {
            // ... existing logic, uses schema_registry ...
        }
        // ... other existing branches ...
    }

    // Delete the invalidate_provider_resolver_cache stub at config_contributions.rs:839-843
    // — it's replaced by the explicit load_from_db calls above.

    Ok(())
}
```

**Update 5 IPC sites in main.rs (lines 8133, 8178, 8281, 8370, 8877):**

Each currently calls `sync_config_to_operational(&writer, &state.pyramid.build_event_bus, &contribution)`. Update to:

```rust
wire_node_lib::pyramid::config_contributions::sync_config_to_operational_with_registry(
    &writer,
    &state.pyramid.build_event_bus,
    &contribution,
    Some(&state.pyramid.schema_registry),
    Some(&state.pyramid.provider_registry),
)
.map_err(|e| e.to_string())?;
```

**Delete 3 dead `registry.load_from_db(conn)` calls in local_mode.rs at lines 471, 573, 775** (Cycle 3 Stage 1 A Issue 13) — they become redundant after the sync dispatcher handles it. Add a comment pointing at the sync dispatcher refresh.

**Delete the `invalidate_provider_resolver_cache` stub at `config_contributions.rs:839-843`.**

**Load-from-db atomicity note:** Cycle 3 Stage 1 B Issue 11 observed that `ProviderRegistry::load_from_db` holds 3 sequential write locks. Not atomic with the preceding upsert, but acceptable for our scenario (mutations are serialized through the writer lock upstream). Document in the plan as "eventually consistent across concurrent IPC mutations; acceptable given the rare-mutation profile."

**Unit tests:**
- `test_sync_tier_routing_refreshes_registry_cache` — populate table via the dispatcher, then `registry.resolve_tier("fast_extract", None, None, None)` returns the new row without a restart.
- `test_sync_step_overrides_refreshes_registry_cache` — same for step overrides.
- `test_sync_config_with_none_registry_still_works` — pass `None, None`, assert the upsert still completes (no registry refresh, but no error either).
- `test_invalidate_provider_resolver_cache_stub_removed` — compile check that the stub function is gone.

---

## Out of Scope

### Deferred to follow-ups after this commit

- **Bug #6** — Phase 17 CC auto-include pulled wrong-directory slugs.
- **Bug #7** — `pyramid_test_api_key` reads legacy config. 5-line follow-up.
- **Bug #8** — Partner `PartnerLlmConfig.api_key` cached at boot.
- **Tier routing Settings modal + 3-way toggle UX** — deferred.
- **`BundledMigrationReport.newly_inserted` tests at 20 call sites** — if the test scaffolding needs to be updated to accept the new field on the report struct, that's in scope; but the plan's Cycle 3 fix AVOIDS signature changes to `walk_bundled_contributions_manifest` itself (Cycle 3 Stage 1 A Issue 12 — 20 test call sites saved).

### Pre-existing bugs documented for future cleanup

- **Bug #10** — `sync.rs` near-miss 600MB pyramid.db POST to newsbleach.com.
- **Bug #11** — `pyramid_config.json` 0644 plaintext.
- **Bug #12** — `stale_local` cloud-pointer lie (resolved: Bug #D deletes the lying row).
- **Bug #13** — `CredentialStore::substitute_to_string` UTF-8 corruption.
- **Bug #14** — `batch_size=1` pinned / Pillar 37 violation.
- **Bug #15** — `ingest_code`/`ingest_docs` no file-size check.
- **Bug #16** — Three inconsistent ignore systems.
- **Bug #17** — Concurrent build dispatch no rate limit.
- **Bug #18** — No observability aggregator.
- **Bug #19** — 2-second sleep coordination primitive.
- **Bug #20** — `ResolvedSecret::drop` zeroize claim.
- **Bug #21** — warp TRACE log noise.
- **Bug #22** — `partner.db-wal` stale.
- **Bug #23** — No pre-flight credential validation.
- **Bug #24** — Test suite codifies broken behavior.
- **Bug #26** — `.credentials.fallback` bypass bootstrap.
- **Bug #27** — `save_ingest_record` upsert blind overwrite (TOCTOU).
- **Bug #29** — `folder_ingestion.rs:290` doc comment lie (fixed in Commit 1).
- **Bug #30** — `skip_dirs` vs `default_ignore_patterns` drift.

### From Cycle 3 Stage 1 (new follow-ups)

- **Reseed migration limitation** — Only fires when the DB matches Adam's specific 3-row broken signature. Users in other broken states (e.g., deleted `fast_extract` manually, 4 rows with different gaps) must re-seed manually via SQL or the deferred Settings UI.
- **`load_from_db` atomicity** — Three sequential write locks, not atomic with preceding upsert. Acceptable under rare-mutation profile; revisit if concurrent IPC mutations become common.
- **`generative_config.rs:233` and `migration_config.rs:567` use `resolve_tier` for cache-key metadata only.** After Bug #D removes the bundled tier_routing, these call sites may query `stale_local` and get `Err` (tier not in table). Need to verify at patch time whether these handle Err gracefully.
- **`chain-step.schema.yaml:19-28`** — `model_tier` field has `options_from: tier_registry`. If Phase 8's YAML-to-UI renderer respects this, the deferred tier routing UI gets a populated dropdown for free. If not, it renders as a text field. Verification deferred.

---

## Execution Plan

### Pre-flight (not a commit)

0. **`rm fix_dispatch.patch`** at repo root.
1. **Move stray `~/` tree** (408 KB, zero-byte pyramid.db) to `/tmp/wire-node-stray-backup-$(date +%s)` and delete the repo-root copy.
2. **Already on `stabilize-main`.** Plan + handoff committed at `9a4c764`. Working tree has three uncommitted ignore-pattern files.

### Commits (7 focused, on `stabilize-main`)

**Commit coherence requirement:** All seven commits are a single logical bundle. Cherry-picking any subset leaves the system in a worse state. Specifically:
- Commit 1 alone: no runtime effect on 65-FAILED.
- Commits 2+3 alone: contributions reach operational tables + sync infrastructure is ready, but resolvers still hardcoded.
- Commits 2+3+4 alone: resolver class fix lands, BUT credentials still missing.
- Commits 2+3+4+5 alone: credentials work, tiers work, DADBEAR still noisy.
- Commits 2+3+4+5+6 alone: DADBEAR quiet, Settings UI credential writes still go to dead legacy field.

**The retry in Commit 5 is ONE-SHOT** — shipping it without Commits 2+3+4 means the retry fires against records that then re-fail on tier or DADBEAR errors, exhausting the retry window.

---

**Commit 1 — `stabilize: bundled ignore patterns + plan + handoff`**

Files:
- `src-tauri/src/pyramid/db.rs` — `default_ignore_patterns()` expansion (already uncommitted)
- `src-tauri/src/pyramid/folder_ingestion.rs` — test additions under `phase17_tests` (already uncommitted) + fix `folder_ingestion.rs:290` doc comment lie about `.wireignore` (Bug #29)
- `src-tauri/assets/bundled_contributions.json` — already-uncommitted pattern list
- `docs/plans/stabilize-main-2026-04-11.md` — this document (already committed as `9a4c764`, amend if needed)
- `docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md` — already committed as `9a4c764`

---

**Commit 2 — `fix(bundled-contributions): two-pass walk syncs operational tables + reconcile tier seed + rename extractor + update VALID_MODEL_TIERS + update CHAIN-DEVELOPER-GUIDE`**

Three architectural reconciliations in one commit — all are "make bundled/seed/code/docs agree":

Files:
- **`src-tauri/src/pyramid/wire_migration.rs`** — extend `BundledMigrationReport` with `newly_inserted: Vec<String>`. Update `walk_bundled_contributions_manifest` to populate it. Add new function `sync_bundled_contributions_to_operational(conn, bus, schema_reg, provider_reg, newly_inserted)` that runs the post-boot pass 2 with per-entry transaction rollback on sync failure.
- **`src-tauri/src/main.rs`** — call `sync_bundled_contributions_to_operational` after `PyramidState` is constructed (~line 9910). Pass bus, both registries, and the walk's `newly_inserted` vec.
- **`src-tauri/assets/bundled_contributions.json`** — delete the `tier_routing` block (Bug #D reconciliation).
- **`src-tauri/src/pyramid/db.rs`** — add `maybe_reseed_tiers_for_bundled_lie(conn)` with signature check + reseed + stale_local lie DELETE. Wire it into the migration sentinel pattern.
- **`chains/defaults/conversation-chronological.yaml`** — rename `extractor → fast_extract` (lines 56, 93, 139 per Cycle 1 verification).
- **`chains/defaults/conversation-episodic.yaml`** — same rename.
- **`chains/defaults/conversation.yaml`** — same rename.
- **`chains/defaults/conversation-episodic-fast.yaml`** — same rename.
- **`chains/defaults/question.yaml`** — same rename.
- **Any `chains/prompts/**/*.yaml`** with `model_tier: extractor` — grep-and-rename.
- **`src-tauri/src/pyramid/chain_engine.rs:379-382`** — update `VALID_MODEL_TIERS` to include all canonical names (Bug #L).
- **`chains/CHAIN-DEVELOPER-GUIDE.md`** — update lines 40, 98, 274 tier documentation (Bug #L).
- **`src-tauri/src/pyramid/db.rs`** or new module — add `scan_chain_tiers_and_warn(chains_dir, conn)` diagnostic. Call from main.rs between `ensure_default_chains` (9742) and `provider_registry.load_from_db` (9832).

Tests:
- `test_walk_bundled_reports_newly_inserted`
- `test_walk_bundled_skips_existing_on_rerun`
- `test_sync_bundled_post_boot_populates_operational`
- `test_sync_bundled_post_boot_rolls_back_on_failure`
- `test_reseed_tiers_signature_check_skips_non_matching_db`
- `test_reseed_tiers_inserts_web_stale_remote_and_deletes_stale_local_lie`
- `test_reseed_tiers_is_idempotent_via_sentinel`
- `test_scan_chain_tiers_warns_on_unknown_tier`

---

**Commit 3 — `fix(config-contributions): add provider_registry parameter to sync_config_to_operational_with_registry + update 5 IPC sites (pure plumbing)`**

Pure plumbing commit. No behavioral change until Commit 4 lands. Separated so the signature change is a clean bisect point.

Files:
- **`src-tauri/src/pyramid/config_contributions.rs`**:
  - Add `provider_registry: Option<&Arc<ProviderRegistry>>` parameter to `sync_config_to_operational_with_registry`.
  - Legacy `sync_config_to_operational` continues to delegate, passing `None, None`.
  - Dispatcher body UNCHANGED (no cache invalidation yet — that's Commit 4).
  - Delete the `invalidate_provider_resolver_cache` stub (unused now, will be replaced in Commit 4).
- **`src-tauri/src/main.rs`**:
  - 5 IPC sites at 8133, 8178, 8281, 8370, 8877 — each currently calls `sync_config_to_operational`. Update to call `sync_config_to_operational_with_registry` with `Some(&state.pyramid.schema_registry), Some(&state.pyramid.provider_registry)`.
  - Note: several of these sites may also need to pass the schema_registry where they currently pass `None`. Verify each site's current schema_registry access at patch time.
- **`src-tauri/src/pyramid/wire_migration.rs`**: `sync_bundled_contributions_to_operational` (added in Commit 2) now passes `Some(schema_reg), Some(provider_reg)` to the dispatcher. Commit 3 just makes this signature match reality — the call site already exists from Commit 2 but assumed the new signature; actual wiring happens here.
- **Test call sites** — approximately 13 sites across `local_mode.rs`, `pyramid_import.rs`, `wire_pull.rs`, `generative_config.rs`, `migration_config.rs`, `config_contributions.rs` tests, `wire_migration.rs` tests, `test_phase9_wanderer.rs`. Update each to either pass the new parameters as `None` (if the test doesn't exercise the cache invalidation path) or `Some(registry)` (if it does).

Tests:
- Compile check that the new signature works at all call sites.
- `test_sync_config_backward_compat_legacy_entry_point` — calling the legacy `sync_config_to_operational` still works.

**Why split from Commit 4:** The signature change touches many files. Keeping it in a dedicated commit makes bisect cleaner. Commit 3 by itself compiles and is a no-op behaviorally — no cache invalidation yet, no resolver wiring, just a parameter added with `None`s passed through. Commit 4 then flips the behavior on top of this plumbing.

---

**Commit 4 — `fix(resolver+cache): wire BOTH resolve_model AND resolve_ir_model to pyramid_tier_routing via registry + context-limit resolvers + additive upsert via unchecked_transaction + cache invalidation on sync`**

THE class fix commit. Wires both the legacy and IR resolver paths, wires both context-limit resolvers, switches the upsert to additive-via-transaction, and turns on cache invalidation in the sync dispatcher (the plumbing for which landed in Commit 3).

Files:
- **`src-tauri/src/pyramid/chain_dispatch.rs`**:
  - Rewrite `resolve_ir_model` at :1023 to the three-layer shape (aliases → registry → legacy fallback).
  - Rewrite `resolve_model` at :186 to the three-layer shape (same pattern, adapted to `(step, defaults, config)` signature).
  - Rewrite `resolve_ir_context_limit` at :1056-1079 to try the registry `TierRoutingEntry.context_limit` first.
  - Rewrite `resolve_context_limit` at :1085-1114 to try the registry first.
  - Update existing `test_resolve_ir_model_*` tests (4 tests at :1716-1772) to account for the new registry-first path. Existing tests construct `LlmConfig::default()` which has `provider_registry: None` — they continue to exercise the legacy-fallback path and stay green.
  - Update existing `test_resolve_model_*` tests (4 tests at :1644-1680) same way.
  - Add NEW tests: `test_resolve_ir_model_registry_wins`, `test_resolve_model_registry_wins`, `test_resolve_ir_context_limit_reads_table`, `test_resolve_context_limit_reads_table`, `test_resolve_*_empty_tier_falls_back_to_primary`.
- **`src-tauri/src/pyramid/db.rs`**:
  - Rewrite `upsert_tier_routing_from_contribution` at :14445 to use `conn.unchecked_transaction()` + loop calling `save_tier_routing`. Preserve pricing synthesis logic. Keep `&Connection` signature. Remove the DELETE statement.
- **`src-tauri/src/pyramid/config_contributions.rs`**:
  - Add `registry.load_from_db(conn)` calls inside `sync_config_to_operational_with_registry` in the `tier_routing` and `step_overrides` branches. Gated on `if let Some(reg) = provider_registry { ... }` so `None` is still safe.
- **`src-tauri/src/pyramid/local_mode.rs`**:
  - Delete the 3 manual `registry.load_from_db(conn)` calls at lines 471, 573, 775. Add comment pointing at the sync dispatcher refresh.

Tests:
- `test_resolve_{ir,}_model_registry_wins_over_fallback`
- `test_resolve_{ir,}_model_aliases_win_over_registry`
- `test_resolve_{ir,}_model_falls_through_on_empty_registry`
- `test_resolve_{ir,}_context_limit_reads_table`
- `test_upsert_tier_routing_is_additive`
- `test_upsert_tier_routing_rolls_back_on_fk_violation`
- `test_upsert_tier_routing_preserves_pricing_synthesis`
- `test_sync_tier_routing_refreshes_registry_cache`

---

**Commit 5 — `fix(credentials): bootstrap from legacy via load_with_bootstrap API + retry`**

Bug #1 fix. Unchanged from prior rev except for the `BootstrapLegacyKey` minimal struct approach per Cycle 3 Stage 1 A Issue 15.

Files:
- `src-tauri/src/pyramid/credentials.rs` — `load_with_bootstrap` + `BootstrapReport` + private `BootstrapLegacyKey` struct + `read_legacy_openrouter_key` helper.
- `src-tauri/src/pyramid/db.rs` — `retry_credential_failed_ingest_records` helper.
- `src-tauri/src/main.rs` — replace `CredentialStore::load` at :9795 with `load_with_bootstrap`. On `bootstrap_report.bootstrapped`, open a short-lived DB connection and run retry.

11 unit tests per the Bug #1 section (plus integration test).

---

**Commit 6 — `fix(dadbear): hoisted Code/Document skip at two layers + delete scope-error tests + 4 emission sites + historical cleanup + IngestSkipped variant in event_bus`**

Bug #2 four-part fix. Corrections from Cycle 3 Stage 1 A Issue 17 (pseudocode variable names) and B Issue 7 (TaggedKind location).

Files:
- `src-tauri/src/pyramid/dadbear_extend.rs` — hoisted skip at top of `dispatch_pending_ingests` AND `run_tick_for_config`. Delete `ContentType::Code | ContentType::Document` arm at :742-748. Delete tests at :1354 and :1388.
- `src-tauri/src/pyramid/db.rs` — `mark_ingest_skipped` helper + historical cleanup migration (37 dadbear_configs + 1082 Phase 0b records).
- **`src-tauri/src/pyramid/event_bus.rs`** (NOT types.rs) — add `IngestSkipped { source_path, reason }` variant to `TaggedKind`.
- `src-tauri/src/pyramid/folder_ingestion.rs` — `maybe_emit_dadbear_config` helper with allow-list semantics. Refactor all 4 emission sites (985, 1058, 1202, 1258).

Tests per Bug #2 Part B.

---

**Commit 7 — `fix(settings-ui): wire pyramid_set_credential + trim + stale-closure + partial-success UX`**

Bug #4. Unchanged from prior rev.

Files:
- `src/components/PyramidSettings.tsx` — handleSave + handleTestApiKey + `autoExecute` dep + `credentialWriteFailed` state + `apiKey.trim()`.
- `src/components/PyramidFirstRun.tsx` — handleSaveApiKey + trim.

---

### Build + install

```bash
cd "/Users/adamlevine/AI Project Files/agent-wire-node"
cd src-tauri && cargo check && cd ..
cargo tauri build
```

Install via `wire-node-build` skill. **Requires Adam's confirmation** before overwriting `/Applications/Wire Node.app`.

**Binary version gate:** After install, verify `CFBundleShortVersionString` in `/Applications/Wire Node.app/Contents/Info.plist` matches `src-tauri/Cargo.toml` version. Mismatch = install failed.

### Clean-boot bootstrap verification

1. `rm -f ~/Library/Application\ Support/wire-node/.credentials`
2. Launch rebuilt app.
3. Verify logs:
   - `grep "Bootstrapped .credentials" wire-node.log` — must match
   - `grep "Reset credential-failed ingest records" wire-node.log` — must match (count=6)
   - `grep "Detected bundled-lie tier state" wire-node.log` — must match
   - `grep "bundled contributions post-boot sync complete" wire-node.log` — must match
4. Verify file perms: `stat -f "%Sp" ~/Library/Application\ Support/wire-node/.credentials` → `-rw-------`
5. Verify tier routing: `sqlite3 pyramid.db "SELECT tier_name, model_id FROM pyramid_tier_routing ORDER BY tier_name"` → 4 rows (`fast_extract`, `stale_remote`, `synth_heavy`, `web`) — `stale_local` deleted.

### Fresh-install simulation

1. Backup: `mv ~/Library/Application\ Support/wire-node ~/Library/Application\ Support/wire-node-pre-fresh-$(date +%s)`
2. Launch cold.
3. First-run wizard fires. Enter known-good key.
4. Verify:
   - `.credentials` created with 0600 + `OPENROUTER_KEY`.
   - `pyramid_config.json.openrouter_api_key` also populated (legacy sync).
   - 4 operational tables populated from bundled contributions (per Bug #25 5 minus dadbear_policy global no-op): `pyramid_build_strategy`, `pyramid_evidence_policy`, `pyramid_folder_ingestion_heuristics`, `pyramid_custom_prompts`.
   - Tier routing is 4 rows from Rust seed: `fast_extract`, `web`, `synth_heavy`, `stale_remote`. No `stale_local`.
5. Pick small test folder, run folder ingest, verify builds succeed with sensible node counts.
6. Restore backup.

### 15-item verification checklist

After clean-boot + fresh-install simulation passes, re-run folder ingest on `/Users/adamlevine/AI Project Files/agent-wire-node/`. Verify ALL of:

1. **New slugs build successfully.** `SELECT COUNT(*) FROM pyramid_slugs WHERE created_at > '<retest-start>' AND node_count > 0` — all rows non-zero.
2. **Conversation slugs recover** (stray CC-1 slugs may still point at wrong path — Bug #6 deferred).
3. **Stale engine recovers** on old pyramids (april9, goodnewseveryone, all-docs).
4. **DADBEAR silent for new slugs** — zero new `DADBEAR: ingest chain dispatch failed` lines.
5. **DADBEAR ingest records clean** — no `pending`/`processing` for retest slugs. Code/Document → `'skipped'`. Conversation → `'complete'`.
6. **Bug #1 retry fired** — log shows `Reset credential-failed ingest records to pending count=6`. 1082 Phase 0b rows deleted by Commit 6 migration.
7. **Tier routing has all seeded tiers** — `SELECT tier_name FROM pyramid_tier_routing` → `fast_extract`, `web`, `synth_heavy`, `stale_remote` (4 rows; `stale_local` absent unless local mode enabled).
8. **Resolver actually uses the table — both paths.** This is the load-bearing verification.

   **Methodology (revised per Cycle 3 Stage 1 A Issue 11 + test false-pass concern):** Enable DEBUG-level logging on `chain_dispatch` module. Run a chain step that uses `model_tier: web` (which maps to `x-ai/grok-4.1-fast` per Rust seed, NOT Adam's `primary_model = inception/mercury-2`). Grep the log for the resolved model:
   ```
   grep -E "(\[IR\]|\[CHAIN\]).*resolved.*web" wire-node.log | tail -5
   ```
   Expected: log shows `grok-4.1-fast` was selected. If the resolver bypassed the table, it would show `inception/mercury-2` (primary_model fallback).

   Repeat for a chain step using the LEGACY path (Adam's actual runtime since `use_ir_executor=false`): watch for `dispatch_llm` / `resolve_model` log output, confirm `grok-4.1-fast` is selected for `web` tier. This test is load-bearing because it distinguishes "resolver consulted table" from "resolver fell through to primary_model which happens to match."

9. **Cache invalidation works** — insert a new row via direct SQL (`INSERT INTO pyramid_tier_routing (tier_name, provider_id, model_id, ...) VALUES ('test_tier_1', 'openrouter', 'test/model', ...)`), then trigger a chain step (or a direct Tauri command exercising `resolve_tier('test_tier_1')`). Row is visible WITHOUT restart. If a restart is needed, the cache invalidation is broken.
10. **No orphaned DADBEAR configs** — `SELECT COUNT(*) FROM pyramid_dadbear_config WHERE content_type IN ('code','document')` = 0.
11. **Bundled ignore patterns take effect** — DB query for slugs/chunks matching `.claude/`, `.lab.bak.`, `~/` patterns → 0 rows.
12. **Settings UI round-trip** — modify key with nonce, save, quit, reopen, verify `.credentials` has modified value.
13. **Orphaned `extractor` tier warnings gone** — `grep "tier references.*extractor.*not in" wire-node.log` = 0 lines after rename.
14. **`.credentials` has correct perms** — `-rw-------`.
15. **Tier discovery scanner WARN log is empty** — post-rename, no chain YAML references unknown tiers.

If any of 1-15 fail, stop and investigate.

### Memory updates

1. Update `feedback_always_scope_frontend.md` with 16-orphan-commands incident reference.
2. Write new `feedback_grep_frontend_for_new_ipc.md`.
3. Write new `feedback_verify_production_call_sites.md` — the `call_model_via_registry` decoration lesson.
4. Write new `feedback_audit_until_clean_three_cycles.md` — 3-cycle retro showing why the `feedback_audit_until_clean` rule saved this commit (Cycle 3 caught the `use_ir_executor` issue that would have made the entire class fix a no-op for Adam).
5. Update MEMORY.md index.

### PR

Once all 15 checklist items pass:
- Title: `stabilize: dual resolver class fix + credential bootstrap + two-pass bundled sync + DADBEAR hoist + Settings UI wire`
- Body: link this plan, 6 bugs + 1 architectural class fix, 7-commit diff, full audit history (3 cycles × 4 auditors = 12 audits).

---

## Success Criteria

1. Six bugs (#1, #2, #4, #25, #D, #L) + class fix (dual resolver wiring + cache invalidation + additive upsert) all have green verifier evidence via the 15-item checklist.
2. No new log errors during retest.
3. Old pyramids resume stale checks.
4. Commit structure is 7 focused commits on a single PR.
5. Clean-boot bootstrap verification passes.
6. Fresh-install simulation passes.
7. Settings UI round-trip confirms credential store writes.
8. Resolver verification (item 8) shows table wins on BOTH legacy and IR paths.
9. Memory updates land in the same session.
10. Adam can run folder ingestion on any folder and get real pyramids.

---

## Assumptions (verified post-Cycle-3)

1. `resolve_ir_model` and `resolve_model` are BOTH live in production. **Verified** — Adam's `use_ir_executor: false` puts him on the legacy path.
2. `sync_config_to_operational_with_registry` takes `SchemaRegistry`, not `ProviderRegistry`. **Verified** — line 637.
3. All 5 IPC sites call the legacy `sync_config_to_operational`. **Verified** — main.rs:8133, 8178, 8281, 8370, 8877.
4. `walk_bundled_contributions_manifest` runs before bus+registry exist. **Verified** — boot order at main.rs:9688/9743/9763/9795/9824/9905.
5. `save_tier_routing` is already an additive upsert. **Verified** — db.rs:12742-12773.
6. `unchecked_transaction()` works on `&Connection`. **Verified** — used idiomatically in delta.rs, stale_engine.rs, db.rs.
7. `TaggedKind` lives in `event_bus.rs`, not `types.rs`. **Verified** — line 37.
8. `chain_engine.rs:379-382` has a `VALID_MODEL_TIERS` list. **Verified**.
9. Four `RegisterDadbearConfig` emission sites in folder_ingestion.rs. **Verified** — 985, 1058, 1202, 1258.
10. `DadbearWatchConfig.content_type` is `String`. **Verified** — types.rs:1487.
11. Tests at dadbear_extend.rs:1354 and :1388 codify broken behavior. **Verified**.
12. `pyramid_tier_routing` schema has NOT NULL on provider_id/model_id. **Verified** — but the plan sidesteps this because the scanner only diagnoses (never inserts NULL rows).
13. `local_mode.rs` has 3 manual `registry.load_from_db(conn)` calls. **Verified** — lines 471, 573, 775.
14. Adam's live DB: 3 rows in `pyramid_tier_routing` (fast_extract, stale_local, synth_heavy → inception/mercury-2). **Verified**.
15. Adam's pyramid_config.json: `use_ir_executor: false, use_chain_engine: true`. **Verified** via direct JSON read.

---

## File Surface

Modified in this commit:

- `src-tauri/src/pyramid/credentials.rs` — Bug #1
- `src-tauri/src/pyramid/dadbear_extend.rs` — Bug #2
- `src-tauri/src/pyramid/db.rs` — Bug #1 retry helper, Bug #2 mark_ingest_skipped + cleanup migration, Bug #D reseed, upsert_tier_routing_from_contribution rewrite, already-uncommitted ignore patterns
- `src-tauri/src/pyramid/folder_ingestion.rs` — Bug #2 emission helper, already-uncommitted tests, Bug #29 doc fix
- `src-tauri/src/pyramid/event_bus.rs` — IngestSkipped variant on TaggedKind
- `src-tauri/src/pyramid/chain_dispatch.rs` — BOTH resolve_model AND resolve_ir_model rewrites + context-limit resolvers + test updates + new tests
- `src-tauri/src/pyramid/config_contributions.rs` — add provider_registry parameter, wire cache invalidation, delete invalidate_provider_resolver_cache stub
- `src-tauri/src/pyramid/wire_migration.rs` — BundledMigrationReport extension, new sync_bundled_contributions_to_operational function
- `src-tauri/src/pyramid/local_mode.rs` — delete 3 dead registry.load_from_db calls
- `src-tauri/src/pyramid/chain_engine.rs` — VALID_MODEL_TIERS update
- `src-tauri/src/main.rs` — load_with_bootstrap wiring + post-bootstrap retry + sync_bundled_contributions_to_operational call + 5 IPC sites updated to sync_config_to_operational_with_registry
- `src-tauri/assets/bundled_contributions.json` — delete tier_routing block, already-uncommitted ignore patterns
- `chains/defaults/conversation-chronological.yaml`, `conversation-episodic.yaml`, `conversation.yaml`, `conversation-episodic-fast.yaml`, `question.yaml` — rename extractor
- Any `chains/prompts/**/*.yaml` with `model_tier: extractor`
- `chains/CHAIN-DEVELOPER-GUIDE.md` — tier documentation update
- `src/components/PyramidSettings.tsx` — Bug #4
- `src/components/PyramidFirstRun.tsx` — Bug #4
- `docs/plans/stabilize-main-2026-04-11.md` — this document (already committed at 9a4c764)
- `docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md` — already committed at 9a4c764
- Memory files under `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/`

Removed in pre-flight:
- `fix_dispatch.patch`
- `~/` repo-root stray tree

READ for verification at patch time (not modified):
- `src-tauri/src/pyramid/question_build.rs`
- `src-tauri/src/pyramid/stale_engine.rs`
- `src-tauri/src/pyramid/llm.rs`
- `src-tauri/src/pyramid/provider.rs`
- `src-tauri/src/pyramid/stale_helpers_upper.rs`
- `src-tauri/src/pyramid/evidence_answering.rs`
- `src-tauri/src/pyramid/openrouter_webhook.rs`
- `src-tauri/src/pyramid/generative_config.rs:233` + `migration_config.rs:567` — verify `resolve_tier` callers handle Err gracefully after tier_routing bundle removal
- `src-tauri/src/pyramid/build_runner.rs` — verify run_chain_build vs run_ir_build dispatch
- `src-tauri/src/pyramid/chain_executor.rs` — verify the 29 call sites around resolve_{ir_,}model after the rewrite
- Tests across the codebase that call `sync_config_to_operational*` — ~13 sites need either `None, None` additions or registry passes
