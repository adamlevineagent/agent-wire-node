# Stabilize Main — 2026-04-11

**Author:** Claude (session with Adam)
**Branch:** `stabilize-main` (to be created off current `main` at `310e34f`)
**Status:** Post-Cycle-2-Stage-1 rewrite + Adam's class-fix direction. Ready for Cycle 2 Stage 2.
**Cycle history:**
- Cycle 1 Stage 1: Bug #2 fix was type-incorrect + semantically broken. Cleanup SQL was unnecessary + incomplete. Bug #1 bootstrap placement missed fallback path. Bug #1 diagnosis wording misattributed the error origin. Corrected.
- Cycle 1 Stage 2: Tier routing table missing `web`/`extractor`/`mid`/`stale_remote` tiers — hard blocker Stage 1 missed. Two tests codify broken Bug #2 behavior and must be deleted. 1088 `failed` ingest records won't self-heal without a retry trigger. Ignore patterns Commit 1 has no runtime effect without supersession (LATER PROVEN WRONG — see Cycle 2). 13 new documented known-issue bugs.
- **Adam pivot (between cycles):** replaced Bug #9 "supersession contribution" approach with a **class fix**: auto-populate tier routing from chain YAML scan + hardcoded Rust tier literals + modal UX for unassigned tiers. Eliminates the entire category of bugs that Cycle 1 Stage 2 introduced.
- Cycle 2 Stage 1: (a) Bug #2 pseudocode had a type error (`DadbearWatchConfig.content_type` is `String`, not `ContentType`); (b) Bug #2 missed a third emission site at `folder_ingestion.rs:1258`; (c) supersession approach for Bug #9 was infrastructurally impossible (`walk_bundled_contributions_manifest` doesn't call `sync_config_to_operational`, and `BundledContributionEntry` has no `supersedes_id` field) — moot because Adam's class fix direction replaces supersession entirely; (d) the "1088 failed records" split was actually **6 credential + 1082 Phase 0b** — retry clause must broaden OR Phase 0b rows must be cleaned up separately; (e) retry call site must be in main.rs, not `load_from_path` (credentials.rs has no DB handle); (f) ignore patterns DO work via Rust fallback — `pyramid_folder_ingestion_heuristics` is empty, `default_ignore_patterns()` is the live source — Commit 1 simplifies substantially; (g) 4/7 tier defaults already exist in `seed_default_provider_registry` — only `mid` and `extractor` are genuinely open (and even those are handled by the modal's blank-state flow); (h) tier scanner must include Rust-code literals (`stale_remote` is only referenced from `stale_helpers_upper.rs`); (i) verification item 11's grep is false-positive-prone + tests wrong ignore system (`.wireignore` via sync.rs coincidentally excludes `.claude/`); (j) commit ordering should put credentials before tier routing OR vice versa in a way that makes each commit visibly move the system forward in bisect; (k) dozens of minor issues. All applied in this rewrite.
- **Adam direction (class fix UX):** MVP frontend — text field per tier row, no 3-way toggle in this commit, full toggle UX deferred to a follow-up commit in the next Wire Node release.

---

## Purpose

Unblock folder ingestion on any folder by fixing four independent bugs and eliminating an entire class of tier-routing bugs via auto-populate. Plus commit the already-uncommitted ignore-pattern work. Plus clean up the repo and stray tree. Then rebuild, install, verify, PR.

"Make main shippable." NOT the Self-Describing Filesystem pivot.

---

## Context

Running folder ingestion on `agent-wire-node/` produced 65 zero-node slugs. Two failure modes in the log: 129 `question build failed ... OPENROUTER_KEY` + 1068 `DADBEAR: ingest chain dispatch failed ... Phase 0b`. But the DB row breakdown for `failed` ingest records is **6 credential + 1082 Phase 0b** — the log count (1068) counts dispatch attempts, the DB count (1082) counts distinct failed records. Different numbers, different meanings.

Root cause analysis showed four orthogonal bugs plus a system-wide architectural gap:

1. `.credentials` file never bootstrapped from legacy `pyramid_config.openrouter_api_key`. Affects chain YAML → `provider.rs::resolve_credential_for` → `credentials.rs::resolve_var` path. Every build fails credential resolution.

2. DADBEAR's `fire_ingest_chain` hard-rejects `Code` and `Document` content types with a Phase 0b stub that was never removed after Phase 17 shipped folder ingestion. DADBEAR's scan tick creates pending records, claims them, dispatches, fails, marks `failed`. Next tick upserts back to `pending`. 1068 log retries.

3. Settings UI (PyramidSettings.tsx, PyramidFirstRun.tsx) writes `apiKey` to legacy `pyramid_config.json` via `pyramid_set_config`. No frontend code calls `pyramid_set_credential`. 16 Phase 3/18 IPC commands have zero frontend callers. User's "re-save the key" attempts silently write nowhere useful.

4. `pyramid_tier_routing` has 3 rows in the live DB (`fast_extract`, `synth_heavy`, `stale_local`) but shipped chain YAMLs reference `mid`, `extractor`, `web`. Rust code references `stale_remote`. After Bug #1 is fixed, builds fail on "tier X is not defined" instead of credential error — same pain, different flavor. The bundled contribution that seeds tier routing destructively deletes tiers not in its list (`upsert_tier_routing_from_contribution`), which is how the 4 tiers got dropped in the first place.

**The architectural class-bug (Bug #9, replacing the old "add missing tiers" fix):** Tier names are user-choosable LLM role labels. Chain YAMLs are the authoritative source of which tier names exist. The DB should auto-discover names from chain YAMLs + Rust literals and prompt the user for model assignments instead of hardcoding or destructively syncing. This fixes Bug #9 AND prevents every future instance of the same class.

---

## The Four Bugs + One Class Fix

### Bug #1 — `.credentials` file never bootstraps from legacy config (P0)

**Error origin:** `credentials.rs:316-328` (`resolve_var`) reached via `provider.rs:958` (bare-variable branch of `resolve_credential_for`). Error text formats the key as `${OPENROUTER_KEY}` regardless of whether the caller used bare or `${...}` form, which is why the original plan misread the origin as a YAML substitution layer.

**Root cause:** Phase 3 added `CredentialStore` as canonical credential source but did not migrate the legacy `pyramid_config.openrouter_api_key` field into it. The synthesized `OpenRouterProvider` fallback in `llm.rs` exists for legacy codepaths that construct `LlmConfig` directly, but production always goes through the provider registry which consults `.credentials` instead.

**Proposed fix (revised per Cycle 2 Stage 1 C5):**

Replace `CredentialStore::load_from_path(path)` with a new API `CredentialStore::load_with_bootstrap(path, data_dir)` that returns `(Self, BootstrapReport)`:

```rust
// Pseudocode
pub struct BootstrapReport {
    pub bootstrapped: bool,
    pub bootstrapped_keys: Vec<String>,
}

impl CredentialStore {
    pub fn load_with_bootstrap(
        path: PathBuf,
        data_dir: &Path,
    ) -> Result<(Arc<Self>, BootstrapReport)> {
        let store = Self::load_from_path_internal(path.clone())?;  // existing logic
        let mut report = BootstrapReport::default();

        // Only bootstrap if this is the primary .credentials path, not the
        // .credentials.fallback footgun — which Bug #26 in Out-of-Scope tracks.
        if path.ends_with(".credentials") {
            if !store.contains("OPENROUTER_KEY") {
                if let Some(key) = read_legacy_openrouter_key(data_dir)? {
                    store.set("OPENROUTER_KEY", &key)
                        .context("bootstrapping OPENROUTER_KEY from legacy")?;
                    report.bootstrapped = true;
                    report.bootstrapped_keys.push("OPENROUTER_KEY".to_string());
                    tracing::info!(
                        "Bootstrapped .credentials with OPENROUTER_KEY from legacy pyramid_config.json"
                    );
                }
            }
        }

        Ok((Arc::new(store), report))
    }
}

fn read_legacy_openrouter_key(data_dir: &Path) -> Result<Option<String>> {
    // Read pyramid_config.json DIRECTLY — do NOT use PyramidConfig::load
    // which silently defaults on parse errors, masking broken user configs.
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
    let value: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "legacy pyramid_config.json is malformed — skipping credential bootstrap. \
                 Fix by hand or via Settings → API Key."
            );
            return Ok(None);
        }
    };

    let legacy = value.get("openrouter_api_key").and_then(|v| v.as_str()).unwrap_or("");
    if legacy.is_empty()
        || legacy.trim() != legacy
        || legacy.starts_with("${")
        || legacy.starts_with('"')
        || legacy.len() < 20
    {
        tracing::debug!("legacy openrouter_api_key is empty or invalid — skipping bootstrap");
        return Ok(None);
    }

    Ok(Some(legacy.to_string()))
}
```

**Main.rs wiring** (per Cycle 2 Stage 1 C5 — retry runs in main.rs, not inside load_from_path):

```rust
// Around main.rs:9794 (replace existing CredentialStore::load call)
let (credential_store, bootstrap_report) = match
    CredentialStore::load_with_bootstrap(
        config.data_dir().join(".credentials"),
        config.data_dir(),
    )
{
    Ok((store, report)) => (store, report),
    Err(e) => {
        tracing::error!(error = %e, "credential store load failed, falling back");
        // existing fallback path at :9806/:9813 stays as-is (Bug #26 tracks its issues)
        (Arc::new(CredentialStore::load_from_path(...).unwrap_or_else(...)), BootstrapReport::default())
    }
};

// Post-bootstrap retry: ONLY fires if bootstrap actually wrote credentials.
if bootstrap_report.bootstrapped {
    match db::open_pyramid_connection(&pyramid_db_path) {
        Ok(conn) => {
            match db::retry_credential_failed_ingest_records(&conn) {
                Ok(count) => tracing::info!(
                    count = count,
                    "Reset credential-failed ingest records to pending after bootstrap"
                ),
                Err(e) => tracing::warn!(error = %e, "failed to retry credential-failed records"),
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to open DB for post-bootstrap retry"),
    }
}
```

**`retry_credential_failed_ingest_records` helper** lives in `db.rs` alongside other ingest-record helpers:

```rust
// Scoped to credential errors only. The 1082 Phase 0b records are handled
// by Commit 4's DADBEAR migration (historical cleanup DELETE), not this retry.
// Broadening the retry to also match Phase 0b would work but is strictly
// less clean than deleting dead historical rows — see Cycle 2 decision in
// Commit 4 below.
pub fn retry_credential_failed_ingest_records(conn: &Connection) -> Result<usize> {
    let affected = conn.execute(
        "UPDATE pyramid_ingest_records
         SET status='pending',
             error_message=NULL,
             updated_at=datetime('now')
         WHERE status='failed'
           AND (error_message LIKE '%config references credential%'
                OR error_message LIKE '%OPENROUTER_KEY%')",
        [],
    )?;
    Ok(affected)
}
```

Expected count on Adam's DB: 6 records reset (the 6 credential-error rows verified via direct SQLite query).

**Unit tests (11):**
1. `.credentials` absent + valid legacy key → bootstrap produces store with `OPENROUTER_KEY`, file at 0600, `BootstrapReport.bootstrapped = true`.
2. `.credentials` present with `OPENROUTER_KEY` → bootstrap no-ops, `BootstrapReport.bootstrapped = false`.
3. `.credentials` absent + no legacy file → empty store, `bootstrapped = false`.
4. `.credentials` absent + malformed JSON → WARN logged, `bootstrapped = false`, does NOT fail.
5. `.credentials` absent + empty legacy → no bootstrap.
6. `.credentials` absent + legacy `"${OPENROUTER_KEY}"` → skip.
7. `.credentials` absent + legacy with trailing whitespace → skip with WARN.
8. `.credentials` absent + legacy length < 20 → skip.
9. **`retry_credential_failed_ingest_records` on a DB with 5 credential-failed + 3 non-credential-failed records → UPDATE returns 5, non-credential rows untouched.**
10. **`retry_credential_failed_ingest_records` on a DB with zero failed records → UPDATE returns 0, no errors.**
11. **Integration test: set up temp data_dir with legacy key, failed credential records in DB; run full `load_with_bootstrap` + retry sequence as main.rs runs; assert `.credentials` exists, records are pending, provider_registry refresh picks up the new key.**

**Path guard against `.credentials.fallback` footgun** (Cycle 2 B m4): The bootstrap check `if path.ends_with(".credentials")` prevents the helper from also firing when the fallback path at `main.rs:9813` uses `.credentials.fallback` as the store path. Tracked as Bug #26 in Out-of-Scope.

**TODO comment:**
```rust
// TODO(self-describing-fs): This legacy bootstrap is a stopgap. Long-term
// direction per feedback_architectural_lens: credential migrations become
// Wire config contributions with supersession. See docs/vision/self-describing-filesystem.md.
```

---

### Bug #2 — DADBEAR Code/Document handling, FOUR-PART FIX (P1)

**Original site:** `dadbear_extend.rs:742-749` (the Phase 0b refusal arm) + `folder_ingestion.rs:985, 1058, 1258` (three `RegisterDadbearConfig` emission sites for Code/Document content types, not two) + tests at `dadbear_extend.rs:1354` and `:1388` that assert the broken behavior.

**Why the Cycle 1 naive fix was wrong:** `fire_ingest_chain` returns `Result<String>` (build_id). `Ok(())` is a type error; `Ok(String::new())` marks records successfully complete with a lying build_id.

**Four-part fix:**

**Part A — Hoisted skip at two layers** (Cycle 2 Stage 1 A C1 + B M1):

`DadbearWatchConfig.content_type` is `String` at `types.rs:1487`, not `ContentType` enum. The hoist must use string comparison:

```rust
// pseudocode — dadbear_extend.rs
// Guard at dispatch_pending_ingests before the claim loop:
if matches!(config.content_type.as_str(), "code" | "document") {
    for record in pending_records {
        if let Err(e) = db::mark_ingest_skipped(
            &conn,
            record.id,
            "handled by folder_ingestion first-build dispatch",
        ) {
            tracing::warn!(record_id = record.id, error = %e, "mark_ingest_skipped failed");
        }
    }
    info!(
        slug = %slug,
        content_type = %config.content_type,
        record_count = pending_records.len(),
        "DADBEAR skipping Code/Document ingest (folder_ingestion-managed)"
    );
    return Ok(());
}
```

**AND** also guard at the scan-tick layer, BEFORE `detect_changes` runs, to prevent the upsert loop that would re-pend records every scan cycle (Cycle 2 B M1):

```rust
// pseudocode — run_tick_for_config
// Guard at the top of the tick, before detect_changes:
if matches!(config.content_type.as_str(), "code" | "document") {
    // Early return — folder_ingestion dispatched the first build and
    // the Self-Describing Filesystem pivot replaces this path.
    return Ok(());
}
```

Two guards cover both the startup case (any existing pending records get skipped on first dispatch) and the recurring case (scan never creates new pending records for Code/Document).

Add `db::mark_ingest_skipped(conn, record_id, reason)` helper with conditional UPDATE to guard against TOCTOU (Cycle 2 B m6):

```rust
pub fn mark_ingest_skipped(conn: &Connection, record_id: i64, reason: &str) -> Result<()> {
    let affected = conn.execute(
        "UPDATE pyramid_ingest_records
         SET status='skipped', error_message=?2, updated_at=datetime('now')
         WHERE id=?1 AND status='pending'",
        params![record_id, reason],
    )?;
    if affected == 0 {
        tracing::warn!(record_id, "mark_ingest_skipped: record not in 'pending' state (raced)");
    }
    Ok(())
}
```

Add a new `IngestSkipped { source_path, reason }` variant to `TaggedKind` in `types.rs` so UI observers see the state transition (Cycle 2 B m3).

**Part B — Delete two test-codification bugs** (Cycle 1 Stage 2 / Cycle 2 Stage 1 re-confirmed):

Delete `test_fire_ingest_chain_code_scope_error` at `dadbear_extend.rs:1354` and `test_fire_ingest_chain_document_scope_error` at `:1388`. These assert the broken Phase 0b refusal is correct — any agent fixing the arm without also deleting them will break `cargo test` and revert the fix.

Replace with new tests:
- `dispatch_pending_ingests` called with `config.content_type = "code"` transitions pending records to `'skipped'` without calling `fire_ingest_chain`.
- `dispatch_pending_ingests` called with `config.content_type = "document"` same.
- `run_tick_for_config` early-returns without calling `detect_changes` for code/document content types.

**Part C — Stop emitting RegisterDadbearConfig for Code/Document at all three sites** (Cycle 2 Stage 1 A C2):

Three sites in `folder_ingestion.rs` emit `RegisterDadbearConfig` for Code/Document content types. The Cycle 1 plan covered only two (985, 1058). Cycle 2 Stage 1 A found the third at line **1258** (CC memory bedrock branch) which hardcodes `ContentType::Document.as_str().to_string()`.

Rather than add the same content-type filter check at three sites, refactor into a helper:

```rust
// pseudocode
fn maybe_emit_dadbear_config(
    ops: &mut Vec<IngestionOperation>,
    slug: String,
    source_path: String,
    content_type: &str,
    scan_interval_secs: u64,
) {
    // Folder-ingest-created Code/Document slugs are built by folder_ingestion's
    // first-build dispatch via question_build::spawn_question_build. DADBEAR
    // doesn't service them. See dadbear_extend.rs guards.
    if matches!(content_type, "code" | "document") {
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

Then refactor lines 985, 1058, and 1258 to call `maybe_emit_dadbear_config(...)` instead of `ops.push(IngestionOperation::RegisterDadbearConfig { ... })` directly.

Line 1202 (CC conversation bedrock, hardcoded `Conversation`) stays unchanged — Conversation is still a valid DADBEAR content type.

Add a unit test: `execute_plan` on a mixed-content folder produces `RegisterDadbearConfig` ops only for Conversation-typed slugs.

**Part D — Historical cleanup migration** (Cycle 2 Stage 1 + Cycle 2 B Issue 4 resolution):

Clean up the existing orphaned state in Adam's live DB in one boot-time migration:

```sql
-- migration: stabilize_main_dadbear_cleanup_v1
-- Runs idempotently. Safe because after Part C, no new rows are created and
-- after Part A, existing rows are not dispatched.

-- Remove DADBEAR configs for Code/Document slugs. 37 rows expected in Adam's DB.
DELETE FROM pyramid_dadbear_config WHERE content_type IN ('code', 'document');

-- Remove historical Phase 0b failed ingest records. 1082 rows expected.
-- These have no path forward:
--   - They can't reset to pending because Bug #1's credential retry only
--     matches credential errors.
--   - They can't be skipped because skipping only happens on new records
--     via the hoisted guards.
--   - They're dead weight in pyramid_ingest_records.
-- Delete is simpler than broadening the retry and creates a cleaner DB state.
DELETE FROM pyramid_ingest_records
WHERE status='failed' AND error_message LIKE '%Phase 0b%';
```

Placed as a one-shot boot migration using the existing `_migration_marker` sentinel pattern at `db.rs:1960-2070`. Key: `created_by='stabilize_main_dadbear_cleanup_v1'`. Idempotent — runs once, then no-ops on subsequent boots.

**Wait-state note:** After migration runs, DADBEAR's tick loop will see 0 Code/Document configs, Part A's scan-tick early-return won't even fire because there's nothing to scan. Clean state.

**TODO comment:**
```rust
// TODO(self-describing-fs): These guards + emission-site filter are a stopgap.
// Folder-ingest Code/Document slugs should never flow through DADBEAR at all.
// Self-Describing Filesystem pivot replaces this path with scan→build-from-checklists.
```

---

### Bug #4 — Settings UI writes to legacy config, never to CredentialStore (P0)

Three sites: `PyramidSettings.tsx:42-61` (handleSave), `:63-85` (handleTestApiKey), `PyramidFirstRun.tsx:27-36` (handleSaveApiKey).

**Plus pre-existing cleanup per `feedback_fix_all_bugs`:**
- Stale closure at `handleSave` dep array — add `autoExecute`.
- Whitespace handling — use `apiKey.trim()` for both legacy and credential calls.
- Partial-success UX — new `credentialWriteFailed` state with explicit error message.
- `PyramidFirstRun.tsx:44` dep array stays `[apiKey]` (no changes) — documented explicitly so implementer doesn't add extraneous deps.
- `handleTestApiKey` dep array stays `[apiKey, authToken, fetchConfig]` — function doesn't read `autoExecute` or `primaryModel`.

**Proposed fix** (applied to all three sites). Note on `handleTestApiKey` per Cycle 2 B M5: the credential write is BEFORE the test call, so a successful test actually reflects what production uses:

```typescript
// PyramidSettings.tsx handleSave
const handleSave = useCallback(async () => {
    setSaving(true);
    setError(null);
    setCredentialWriteFailed(false);
    const trimmedApiKey = apiKey.trim();
    try {
        await invoke('pyramid_set_config', {
            ...(trimmedApiKey ? { apiKey: trimmedApiKey } : {}),
            ...(authToken ? { authToken } : {}),
            ...(primaryModel ? { primaryModel } : {}),
            autoExecute,
        });
        if (trimmedApiKey) {
            try {
                await invoke('pyramid_set_credential', {
                    key: 'OPENROUTER_KEY',
                    value: trimmedApiKey,
                });
            } catch (credErr) {
                setCredentialWriteFailed(true);
                setError(
                    `Legacy config saved, but writing to credential store failed: ${credErr}. ` +
                    `Chain builds may still fail until this is resolved. ` +
                    `Check .credentials file permissions.`
                );
                return;
            }
        }
        setSaved(true);
        setTimeout(() => setSaved(false), 2000);
        await fetchConfig();
        setPrimaryModel('');
    } catch (err) {
        setError(String(err));
    } finally {
        setSaving(false);
    }
}, [apiKey, authToken, primaryModel, autoExecute, fetchConfig]);
// ^ NEW: autoExecute added to deps (pre-existing stale closure fix).

// PyramidSettings.tsx handleTestApiKey
const handleTestApiKey = useCallback(async () => {
    if (!apiKey.trim()) { setTestResult('Enter an API key first'); return; }
    setTesting(true);
    setTestResult(null);
    const trimmedApiKey = apiKey.trim();
    try {
        // Save to legacy field first (existing behavior)
        await invoke('pyramid_set_config', {
            apiKey: trimmedApiKey,
            ...(authToken ? { authToken } : {}),
        });
        // Save to credential store BEFORE testing so the test reflects
        // post-save state. Non-blocking on failure: if credential write
        // fails, test still runs against the legacy field and at least
        // gives feedback on whether the key is valid at all.
        try {
            await invoke('pyramid_set_credential', {
                key: 'OPENROUTER_KEY',
                value: trimmedApiKey,
            });
        } catch (credErr) {
            // Log the credential write failure but continue with the test.
            // Test will use the legacy field.
            setTestResult(`(Warning: credential store write failed: ${credErr}) ...`);
        }
        const result = await invoke<string>('pyramid_test_api_key');
        setTestResult((prev) => (prev ? `${prev} ${result}` : result));
        await fetchConfig();
    } catch (err) {
        setTestResult(`Test failed: ${err}`);
    } finally {
        setTesting(false);
    }
}, [apiKey, authToken, fetchConfig]);
// ^ No dep array changes: handleTestApiKey doesn't read autoExecute or primaryModel.

// PyramidFirstRun.tsx handleSaveApiKey
const handleSaveApiKey = useCallback(async () => {
    const trimmedApiKey = apiKey.trim();
    if (!trimmedApiKey) { setError('Enter an API key'); return; }
    setSaving(true);
    try {
        await invoke('pyramid_set_config', {
            apiKey: trimmedApiKey,
            authToken: '',
        });
        await invoke('pyramid_set_credential', {
            key: 'OPENROUTER_KEY',
            value: trimmedApiKey,
        });
        onComplete();
    } catch (err) {
        setError(`Save failed: ${err}`);
    } finally {
        setSaving(false);
    }
}, [apiKey, onComplete]);
// ^ No dep array changes beyond existing: still [apiKey].
```

**`authToken` scope resolution:** confirmed out of scope. Reads only from `LlmConfig.auth_token` for local Vibesmithy HTTP route auth. No parallel `pyramid_set_credential` call needed.

**Known-out-of-scope caveat on `pyramid_test_api_key`:** Bug #7 — the IPC at `main.rs:5691-5715` reads `state.pyramid.config.api_key` (legacy) not CredentialStore. After Bug #4 lands, "Test Key" can still show "valid" on a key that the credential store doesn't have. Follow-up fix. The handleTestApiKey flow above saves to BOTH legacy and credential store BEFORE calling test, so the legacy field is fresh enough to give a meaningful test result. Acceptable stopgap.

**TODO comment:**
```typescript
// TODO(credentials-ui-refactor): Stopgap. Long-term UI is a generic credentials
// manager driven by pyramid_list_credentials + pyramid_set_credential. 16 Phase
// 3/18 IPC commands are orphaned. See feedback_grep_frontend_for_new_ipc.md.
```

---

### Bug #9 — Tier routing CLASS FIX: auto-populate from chain YAML + Rust literals, additive upsert, MVP modal (P0)

**Replacing the original "missing tiers + supersession contribution" approach** which Cycle 2 Stage 1 proved infrastructurally impossible (`walk_bundled_contributions_manifest` doesn't call `sync_config_to_operational`; `BundledContributionEntry` has no `supersedes_id`).

**Architectural direction (Adam green-lit):** Tier names are user-choosable LLM role labels. Chain YAMLs are the authoritative source of which names exist. The DB should auto-discover names from chain YAMLs + Rust literals and prompt the user for model assignments instead of hardcoding or destructively syncing.

**Four-part fix:**

**Part A — `upsert_tier_routing_from_contribution` becomes additive** at `db.rs:14445-14470`:

Current behavior: DELETE any tier not in the incoming contribution, INSERT new tiers, UPDATE existing ones. Destructive — how Adam's DB lost `web`, `extractor`, `mid`, `stale_remote` when a user-customized contribution only named 3 tiers.

New behavior: NEVER DELETE. For each tier in the incoming contribution, INSERT OR UPDATE. Leave every existing row alone. Add a WARN log when a contribution supersede-accept-flow deliberately removes a tier (via a new `tier_removed` field or equivalent explicit signal — out of scope for this commit, just leave the warn hook for future).

```rust
// pseudocode — db.rs:14445+
pub fn upsert_tier_routing_from_contribution(
    conn: &Connection,
    contribution_id: &str,
    tier_rows: &[TierRoutingRow],
) -> Result<usize> {
    // ADDITIVE: never delete tiers not in the incoming set. A contribution
    // that wants to remove a tier must signal it explicitly via a future
    // 'removed_tiers' field (tracked as Bug #28).
    let mut affected = 0;
    for row in tier_rows {
        conn.execute(
            "INSERT INTO pyramid_tier_routing (tier_name, provider_id, model_id, ...)
             VALUES (?1, ?2, ?3, ...)
             ON CONFLICT(tier_name) DO UPDATE SET
               provider_id = excluded.provider_id,
               model_id = excluded.model_id,
               ...",
            params![row.tier_name, row.provider_id, row.model_id, ...],
        )?;
        affected += 1;
    }
    tracing::info!(
        contribution_id = %contribution_id,
        tier_count = affected,
        "upsert_tier_routing_from_contribution: merged tiers (additive)"
    );
    Ok(affected)
}
```

**Part B — Boot-time tier scanner** discovers every tier name referenced anywhere:

```rust
// pseudocode — new module db.rs or dedicated tier_discovery.rs
use std::collections::HashSet;
use std::path::Path;

// Hardcoded list of tier literals referenced directly from Rust code.
// Keep this in ONE place instead of scattered `"fast_extract".to_string()` calls.
// Source of truth for all Rust-code tier references.
pub const RUST_REFERENCED_TIERS: &[&str] = &[
    "fast_extract",  // evidence_answering.rs, llm.rs, openrouter_webhook.rs
    "stale_remote",  // stale_helpers_upper.rs:2171, :4530 — Cycle 2 Stage 1 A C7
    // (scan code at patch time for any more — grep for "with_model_resolution(" and tier_name literals)
];

pub fn discover_referenced_tiers(
    chains_dir: &Path,
    conn: &Connection,
) -> Result<HashSet<String>> {
    let mut tiers: HashSet<String> = RUST_REFERENCED_TIERS
        .iter()
        .map(|s| s.to_string())
        .collect();

    // Walk chains/defaults/*.yaml and chains/prompts/**/*.yaml
    for entry in walkdir::WalkDir::new(chains_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().map(|x| x == "yaml").unwrap_or(false))
    {
        let yaml = match fs::read_to_string(entry.path()) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for line in yaml.lines() {
            // Loose regex: `model_tier:` followed by an identifier
            if let Some(tier_name) = parse_model_tier_value(line) {
                tiers.insert(tier_name);
            }
        }
    }

    // Also walk chain contributions stored in pyramid_config_contributions.
    // They're YAML text stored as 'yaml_content' column.
    let chain_contributions = conn.prepare(
        "SELECT yaml_content FROM pyramid_config_contributions
         WHERE status='active' AND schema_type IN ('chain', 'chain_binding')",
    )?.query_map([], |row| row.get::<_, String>(0))?;

    for yaml_result in chain_contributions {
        if let Ok(yaml) = yaml_result {
            for line in yaml.lines() {
                if let Some(tier_name) = parse_model_tier_value(line) {
                    tiers.insert(tier_name);
                }
            }
        }
    }

    Ok(tiers)
}

fn parse_model_tier_value(line: &str) -> Option<String> {
    // Match "model_tier: foo" or "    model_tier: foo  # comment"
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("model_tier:") {
        let value = rest.trim().split_whitespace().next()?;
        // Guard against YAML variable references like $item.tier
        if value.starts_with('$') || value.starts_with('{') {
            return None;
        }
        Some(value.to_string())
    } else {
        None
    }
}
```

**Part C — Auto-populate missing tiers on boot** with `status='unassigned'` (NULL provider_id + NULL model_id):

```rust
// pseudocode — called from main.rs boot sequence after seed_default_provider_registry
pub fn ensure_all_referenced_tiers_populated(
    conn: &Connection,
    discovered: &HashSet<String>,
) -> Result<usize> {
    let existing: HashSet<String> = conn
        .prepare("SELECT tier_name FROM pyramid_tier_routing")?
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(Result::ok)
        .collect();

    let mut new_count = 0;
    for tier_name in discovered {
        if !existing.contains(tier_name) {
            // Insert with NULL provider_id + NULL model_id as the "unassigned"
            // marker. pyramid_tier_routing schema may need migration to allow
            // nullable provider_id/model_id if it currently enforces NOT NULL —
            // verify at patch time. Alternative: introduce a sentinel string
            // like "__unassigned__" but that's worse (schema-lies).
            conn.execute(
                "INSERT INTO pyramid_tier_routing
                 (tier_name, provider_id, model_id, notes, created_at)
                 VALUES (?1, NULL, NULL, ?2, datetime('now'))",
                params![
                    tier_name,
                    format!("auto-discovered from chain scan — awaiting user assignment")
                ],
            )?;
            new_count += 1;
            tracing::info!(
                tier = %tier_name,
                "auto-populated tier with unassigned status"
            );
        }
    }
    Ok(new_count)
}
```

**Schema verification at patch time:** confirm that `pyramid_tier_routing.provider_id` and `model_id` allow NULL. If the current schema has `NOT NULL` constraints, add a schema migration (additive, ALTER TABLE ... or recreate-with-copy) to allow nullable. If that's a no-go, use a sentinel string. Decision deferred to patch time.

**`resolve_tier` error message upgrade:**

```rust
// In chain_executor.rs or wherever resolve_tier is called — find at patch time
// Old error: "tier 'X' is not defined in pyramid_tier_routing"
// New error, when the tier exists but is unassigned (NULL model_id):
Err(anyhow!(
    "Tier '{}' is unassigned — no model has been set for this role. \
     Click the banner at the top of the app to assign a model in Settings → Tier Routing.",
    tier_name
))
```

**Lift defaults from `seed_default_provider_registry`:**

At `db.rs:12908-12996`, the seed function already has Adam-blessed defaults for 4 of the 6 required tiers. Part C runs AFTER the seed, so these rows are already populated when the scanner runs and won't get touched by `ensure_all_referenced_tiers_populated`:

- `fast_extract` → `openrouter|inception/mercury-2` (Adam's default)
- `web` → `openrouter|x-ai/grok-4.1-fast` (Adam's default)
- `synth_heavy` → `openrouter|minimax/minimax-m2.7` (Adam's default)
- `stale_remote` → `openrouter|minimax/minimax-m2.7` (Adam's default)

These are in the bundled contribution ALREADY (verified via `seed_default_provider_registry` doc comments). But the user's live DB has only 3 rows because the destructive supersession wiped the others. Part A + Part C together recover this: on boot, the seed has run (no-op if rows exist), the scanner detects `mid`, `extractor`, `web`, `stale_remote`, and the populator inserts rows for any not present.

**For Adam's live DB specifically:** after Parts A/B/C run, expected final state:
- `fast_extract` → Adam's existing row (no change)
- `stale_local` → Adam's existing row (preserved via additive upsert; cloud-pointer lie stays — user can fix via modal if they want)
- `synth_heavy` → Adam's existing row (no change)
- `web` → auto-populated as NULL/NULL (unassigned)
- `mid` → auto-populated as NULL/NULL (unassigned)
- `extractor` → auto-populated as NULL/NULL (unassigned)
- `stale_remote` → auto-populated as NULL/NULL (unassigned)

4 unassigned tiers trigger the modal on next boot. Adam fills them in. Done.

Alternative: **pre-populate `web`, `stale_remote` with Adam's blessed defaults** directly in the scanner/populator code. These values are already in the seed function as comments. If the schema supports it, the populator uses these defaults when inserting. Then the modal only has `mid` and `extractor` to prompt for — minimal interruption. **Adam preferred the modal to handle unassigned tiers so the user confronts every decision, so skip the pre-populate and show all 4 in the modal.** Revisit at patch time if the modal feels tedious.

**Part D — MVP frontend: tier routing modal + banner**

MVP per Adam's direction: text field per tier row. No 3-way provider toggle yet (deferred to a follow-up commit in the next Wire Node release). Scope: ~150-200 lines TSX.

New component `src/components/PyramidTierRouting.tsx`:
- Fetches all tiers via `pyramid_list_tier_routing` IPC (backend already exists, orphaned).
- Renders a table: `tier_name` | `model_id (editable)` | `notes` | action button.
- Rows with NULL `model_id` are highlighted red ("Unassigned — set a model").
- Rows with values are editable but not required to change.
- Single text field per row for `"provider_id|model_id"` format (e.g., `"openrouter|inception/mercury-2"`).
- "Save All" button dispatches `pyramid_save_tier_routing` for each changed row.
- Input validation: reject empty, reject missing `|`, reject whitespace.
- On save success, refresh the list.
- Fires `IngestRetrigger` (or equivalent) after save so the retry of failed records kicks off (chain flow improvements).

New component `src/components/TierAssignmentBanner.tsx`:
- Shows at the top of the app when any tier has NULL `model_id`.
- Message: "N tier(s) need a model assigned. Chains referencing them will fail until fixed."
- Button: "Assign now" → opens the tier routing modal.
- Queries tier state on mount + listens for updates.

New boot-time modal trigger in `App.tsx` or equivalent root:
- On mount, invoke `pyramid_list_tier_routing`.
- If any row has NULL `model_id`, auto-open the tier routing modal.
- Dismissable with "Later" button, which closes the modal but keeps the banner visible.

New hook `src/hooks/useTierRouting.ts`:
- Wraps `pyramid_list_tier_routing`, `pyramid_save_tier_routing` (already exist as orphaned Tauri commands).
- Returns `{ tiers, unassignedCount, refresh, save }`.

**Backend IPC handlers:** `pyramid_list_tier_routing` and `pyramid_save_tier_routing` already exist per the Cycle 1 findings (16 orphan commands). Verify signatures at patch time; if either doesn't exist, add it.

**TODO comments:**
```rust
// TODO(tier-routing-ux-v2): This modal is MVP. Next sprint: 3-way provider
// toggle (openrouter/local/other), Ollama dropdown from pyramid_probe_ollama,
// model slug autocomplete. See Adam's direction in session notes 2026-04-11.
```

```typescript
// TODO(tier-routing-ux-v2): MVP text field. Replace with 3-way toggle UI
// matching the useLocalMode hook's provider shape. Next Wire Node release.
```

**Unit tests:**
- `discover_referenced_tiers` on a test dir with synthetic chains → returns expected set.
- `ensure_all_referenced_tiers_populated` on a test DB → inserts missing, leaves existing alone.
- `upsert_tier_routing_from_contribution` called with a 3-tier contribution on a 5-tier DB → 5 tiers remain, 3 updated (not 3 total).

---

## Out of Scope (Explicit)

### Deferred known bugs from Cycle 1 and Cycle 2 audits

- **Bug #3** — `question_build::spawn_question_build` is the general first-build dispatcher. Intentional. TODO comment only.
- **Bug #5 (retracted)** — Stale engine "hot loop" was parallel fan-out.
- **Bug #6 (deferred)** — Phase 17 CC auto-include over-scoping pulled wrong directory as conversation source. 3 stray slugs in live DB pointing at `agentwirenodetestclaudesplusmem/foldervine-test`. Post-stabilize-main.
- **Bug #7** — `pyramid_test_api_key` reads legacy config, not CredentialStore. 5-line follow-up.
- **Bug #8** — partner `PartnerLlmConfig.api_key` cached at boot, never refreshed.

### From Cycle 1 Stage 2 (unchanged from prior rev)

- **Bug #10** — `sync.rs` near-miss data exfiltration: tried to POST 600MB `pyramid.db` to `newsbleach.com` (failed on body size limit). `scan_local_folder` uses only `.gitignore`/`.wireignore`. Needs denylist + size cap. Security-critical; separate branch.
- **Bug #11** — `pyramid_config.json` is 0644 plaintext with OpenRouter API key + auth token. Migrate to CredentialStore.
- **Bug #12** — `stale_local` tier lies about being local (cloud OpenRouter pointer). Partially addressed by modal allowing user re-assignment.
- **Bug #13** — `CredentialStore::substitute_to_string` corrupts multibyte UTF-8 via byte-cast to char.
- **Bug #14** — `batch_size=1` pinned at 8+ sites / Pillar 37 violation.
- **Bug #15** — `ingest_code`/`ingest_docs` read entire files into memory with no size check.
- **Bug #16** — Three inconsistent ignore systems drift.
- **Bug #17** — Concurrent question builds with no rate limit.
- **Bug #18** — No observability aggregator for mass identical errors.
- **Bug #19** — 2-second sleep as coordination primitive in `spawn_initial_builds`.
- **Bug #20** — `ResolvedSecret::drop` zeroize claim is false.
- **Bug #21** — warp TRACE log noise inflating log file.
- **Bug #22** — `partner.db-wal` 3 weeks stale with 1.6MB uncommitted.
- **Bug #23** — No pre-flight credential validation before dispatching builds.
- **Bug #24** — Test suite codifies broken behavior (class pattern; the two specific tests at `dadbear_extend.rs:1354, 1388` ARE fixed in Commit 4).

### NEW from Cycle 2 Stage 1

- **Bug #25** — `walk_bundled_contributions_manifest` at `wire_migration.rs:1044-1089` inserts bundled contributions into `pyramid_config_contributions` but NEVER calls `sync_config_to_operational`. Means bundled contributions become contribution rows but don't affect operational tables. Only user actions / wire pull / local mode toggle trigger the sync. Architectural hole that this plan routes around (via direct-upsert for tier routing, Rust fallback for ignore patterns). Fix: modify `walk_bundled_contributions_manifest` to call `sync_config_to_operational_with_registry` on every newly-inserted row. Separate branch.
- **Bug #26** — `main.rs:9813` `.credentials.fallback` path is a bogus fallback. If primary load errors, the fallback writes to `.credentials.fallback` which nothing else reads. Bug #1's bootstrap is guarded against this (path check) but the underlying fallback is still broken. Fix: remove the fallback or make it non-writable. Separate branch.
- **Bug #27** — `save_ingest_record`'s upsert blindly overwrites status via `excluded.status`. A record transitioned to `'skipped'` by the hoisted guard can ping-pong back to `pending` on next scan if the file changes. Part A's scan-tick early-return (in Bug #2) prevents this for Code/Document, but the pattern is a latent footgun for any future content type using the skipped status. Fix: make `save_ingest_record`'s upsert preserve existing terminal states. Separate branch.
- **Bug #28** — `upsert_tier_routing_from_contribution` destructively deleted tiers. Part A of Bug #9 makes it additive. Need a separate "removed_tiers" signal for future contributions that want to explicitly remove tiers. Follow-up.
- **Bug #29** — `folder_ingestion.rs:290` doc comment falsely claims `.wireignore` is honored by the walker. It isn't. Doc-only fix. Can fold into Commit 1 if trivial.
- **Bug #30** — `skip_dirs()` in `ingest.rs:55-71` drifts from `default_ignore_patterns()`. Partial fix: unify in Commit 1 if scope allows (one-line change — have `ingest_code`/`ingest_docs` use `default_ignore_patterns` via `path_matches_any_ignore` instead of the separate `skip_dirs`). Decide at patch time.

### Architectural

- **Self-Describing Filesystem pivot** — next initiative.
- **Generic credentials / providers / tiers Settings panels** — long-term UX replacing the MVP.
- **3-way tier routing toggle UX (openrouter/local/other)** — follow-up after this commit. Next Wire Node release.
- **Wire contribution-as-credential-migration** — long-term per `feedback_architectural_lens`.
- **15+ other orphan Phase 3/18 IPC commands** — long-term.

---

## Execution Plan

### Pre-flight (no commit, no branch yet)

0. **`rm fix_dispatch.patch`** at repo root.

1. **Remove the stray `~/Library/…` tree** at the repo root. Verified 408 KB with zero-byte pyramid.db — no meaningful work to preserve (Cycle 2 Stage 1 A Issue 14). Still move aside to `/tmp/wire-node-stray-backup-$(date +%s)/` first as standard safety. Adam's explicit confirmation is a formality given zero-byte payload.

2. **Commit the untracked handoff doc** — `docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md`. Goes in Commit 1.

3. **Create branch:** `git checkout -b stabilize-main`.

4. **NO manual `.credentials` write** — skipped. The fix lands via rebuild+install and clean-boot verification.

### Commits (5 focused, on `stabilize-main` branch, merge as one PR)

**Commit coherence requirement:** All five commits are a single logical bundle. Cherry-picking any subset leaves the system in a worse state. Specifically:
- Commit 1 alone: no effect on 65-FAILED.
- Commit 2 alone: tiers auto-populate but credentials still fail.
- Commit 3 alone (credentials): builds move from "credential error" to "tier not assigned" modal prompt.
- Commit 4 alone: DADBEAR silent but Code/Document slugs still fail at credential/tier resolution.
- Commit 5 alone: Settings UI can write credentials but bootstrap + tier routing still broken.

**The retry in Commit 3 is ONE-SHOT.** If shipped without Commits 2+4, the retry fires, resets 6 credential records to pending, they fail again with "tier not assigned", the updated `error_message` no longer matches the LIKE clause, and the retry is exhausted. Never cherry-pick Commit 3 alone.

**Commit 1 — `stabilize: bundled ignore patterns + handoff doc`**

Simple. Commit the already-uncommitted work + the handoff doc.

Files:
- `src-tauri/src/pyramid/db.rs` — `default_ignore_patterns()` expansion (already uncommitted). Takes effect via Rust fallback because `pyramid_folder_ingestion_heuristics` operational table is empty per Cycle 2 Stage 1 verification. NO contribution supersession needed.
- `src-tauri/src/pyramid/folder_ingestion.rs` — test additions under `phase17_tests` (already uncommitted).
- `src-tauri/assets/bundled_contributions.json` — already-uncommitted pattern list (seeds for future clean installs; no effect on Adam's DB until he deletes and re-bootstraps).
- `docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md` — untracked handoff doc.
- `src-tauri/src/pyramid/folder_ingestion.rs:290` — fix the doc-comment lie about `.wireignore` being honored (Bug #29, trivial fold).
- Optional if scope allows: unify `ingest.rs::skip_dirs()` with `default_ignore_patterns()` via `path_matches_any_ignore` (Bug #30 partial fix). Decide at patch time based on test surface.

No new tests beyond the already-uncommitted ones. `cargo test --lib test_path_matches_any_ignore` already passes.

**Commit 2 — `fix(tier-routing): class fix via auto-populate scanner + additive upsert + MVP modal`**

Bug #9 class fix. Backend + frontend coupled (ship together per `feedback_always_scope_frontend`).

Backend:
- `src-tauri/src/pyramid/db.rs`:
  - `RUST_REFERENCED_TIERS: &[&str]` const listing `fast_extract`, `stale_remote`, and any others found via grep at patch time.
  - `fn discover_referenced_tiers(chains_dir, conn) -> HashSet<String>` — walks `chains/defaults/*.yaml` + chain contributions in DB + `RUST_REFERENCED_TIERS`.
  - `fn parse_model_tier_value(line) -> Option<String>` helper with guards against `$` and `{` variable references.
  - `fn ensure_all_referenced_tiers_populated(conn, discovered) -> usize` — INSERT missing tiers with NULL provider_id/model_id.
  - `upsert_tier_routing_from_contribution` rewritten to be additive (never DELETE rows not in incoming set).
  - WARN log in `upsert_tier_routing_from_contribution` when a future `removed_tiers` field is present (stub for Bug #28).
  - Schema migration if needed to allow nullable `provider_id`/`model_id` on `pyramid_tier_routing`. Verify at patch time.
- `src-tauri/src/pyramid/chain_executor.rs` (or wherever `resolve_tier` lives):
  - Upgrade error message: "Tier '{X}' is unassigned — no model has been set for this role. Click the banner at the top of the app to assign a model in Settings → Tier Routing."
- `src-tauri/src/main.rs` (boot sequence, after `seed_default_provider_registry`):
  - Call `discover_referenced_tiers(&chains_dir, &conn)`.
  - Call `ensure_all_referenced_tiers_populated(&conn, &discovered)`.
  - Log the count of auto-populated tiers.

Frontend:
- `src/hooks/useTierRouting.ts` — new hook wrapping `pyramid_list_tier_routing` + `pyramid_save_tier_routing` IPCs.
- `src/components/PyramidTierRouting.tsx` — new modal component. Shows ALL tiers with assigned ones editable and unassigned ones required. Text field per row for `"provider_id|model_id"`. Save All button dispatches updates. Input validation (non-empty, contains `|`).
- `src/components/TierAssignmentBanner.tsx` — new persistent banner. Shows when any tier has NULL `model_id`. Opens the modal.
- Integration into app root (`App.tsx` or equivalent): boot-time check, auto-open modal, register banner.
- `src/components/Settings.tsx` — add a "Tier Routing" tab/section that opens the same modal (so users can re-edit later).

Unit tests:
- `parse_model_tier_value` — happy path, skips `$foo`, skips `{foo}`, handles comments.
- `discover_referenced_tiers` — test dir with synthetic chains → expected set. Includes Rust literals.
- `ensure_all_referenced_tiers_populated` — empty DB + 5 discovered tiers → 5 rows inserted. DB with 3 tiers + 5 discovered (2 new, 3 overlap) → 2 new rows.
- `upsert_tier_routing_from_contribution` — 3-tier DB + 2-tier contribution → 3 rows remain (2 updated, 1 untouched). NOT 2 rows.

**Commit 3 — `fix(credentials): bootstrap via load_with_bootstrap API + gated retry`**

Bug #1 fix.

- `src-tauri/src/pyramid/credentials.rs`:
  - New `pub fn load_with_bootstrap(path, data_dir) -> Result<(Arc<Self>, BootstrapReport)>`.
  - New `BootstrapReport { bootstrapped: bool, bootstrapped_keys: Vec<String> }` struct.
  - Private helper `read_legacy_openrouter_key(data_dir) -> Result<Option<String>>` — direct fs+serde read with validation.
  - Path guard: only fire bootstrap when `path.ends_with(".credentials")`.
- `src-tauri/src/pyramid/db.rs`:
  - `pub fn retry_credential_failed_ingest_records(conn) -> Result<usize>` — UPDATE with credential-only LIKE clause.
- `src-tauri/src/main.rs`:
  - Replace `CredentialStore::load(...)` call at `:9794` with `CredentialStore::load_with_bootstrap(...)`.
  - Post-load: if `bootstrap_report.bootstrapped`, open a short-lived DB connection and call `retry_credential_failed_ingest_records`.
  - Log the retry count.

11 unit tests per the Bug #1 section above, including the integration test (#11).

**Commit 4 — `fix(dadbear): hoisted Code/Document skip at two layers + delete scope-error tests + third emission site + historical cleanup`**

Bug #2 four-part fix + historical migration.

- `src-tauri/src/pyramid/dadbear_extend.rs`:
  - Add hoisted skip check in `dispatch_pending_ingests` using `config.content_type.as_str()` (NOT enum matching — type error otherwise).
  - Add early-return in `run_tick_for_config` for Code/Document content types.
  - Delete the unreachable `ContentType::Code | ContentType::Document` arm at `:742-748`.
  - **DELETE `test_fire_ingest_chain_code_scope_error` at `:1354` and `test_fire_ingest_chain_document_scope_error` at `:1388`.**
  - Add new tests: hoisted skip releases claims to `'skipped'` status, tick early-return for Code/Document, no `fire_ingest_chain` call.
- `src-tauri/src/pyramid/db.rs`:
  - Add `mark_ingest_skipped(conn, record_id, reason)` helper with conditional UPDATE `WHERE id=?1 AND status='pending'` (TOCTOU-safe).
  - Add `'skipped'` handling note (pyramid_ingest_records.status has no CHECK constraint, so schema-legal).
- `src-tauri/src/pyramid/types.rs`:
  - Add `IngestSkipped { source_path: String, reason: String }` variant to `TaggedKind`.
- `src-tauri/src/pyramid/folder_ingestion.rs`:
  - Refactor all THREE `IngestionOperation::RegisterDadbearConfig` emission sites (lines 985, 1058, 1258) through a `maybe_emit_dadbear_config(...)` helper that skips Code/Document.
  - Unit test: `execute_plan` on mixed-content folder produces configs only for Conversation slugs.
- **Historical cleanup migration** (one-shot, gated by `_migration_marker` with `created_by='stabilize_main_dadbear_cleanup_v1'`):
  - `DELETE FROM pyramid_dadbear_config WHERE content_type IN ('code', 'document');` — 37 rows expected in Adam's DB.
  - `DELETE FROM pyramid_ingest_records WHERE status='failed' AND error_message LIKE '%Phase 0b%';` — 1082 rows expected.

**Commit 5 — `fix(settings-ui): wire credential form + trim + stale-closure + partial-success UX`**

Bug #4 fix. Per the pseudocode in the Bug #4 section above.

Modify:
- `src/components/PyramidSettings.tsx` — handleSave + handleTestApiKey.
- `src/components/PyramidFirstRun.tsx` — handleSaveApiKey.
- Add TODO comments at all three sites.

### Build + install

```
cd "/Users/adamlevine/AI Project Files/agent-wire-node"
cd src-tauri && cargo check && cd ..
cargo tauri build
```

`cargo check` runs in `src-tauri/` (manifest location) per Cycle 2 Stage 1 A Issue 20. NOT `--lib` per `feedback_cargo_check_lib_insufficient_for_binary`.

Install via `wire-node-build` skill: copy `src-tauri/target/release/bundle/macos/Wire Node.app` to `/Applications/Wire Node.app`. **Requires Adam's explicit confirmation.**

**Binary version gate (Cycle 2 Stage 1 B M9):** After install, query `/Applications/Wire Node.app/Contents/Info.plist` CFBundleShortVersionString and compare to `env!("CARGO_PKG_VERSION")` in `src-tauri/Cargo.toml`. Match = proceed. Mismatch = install failed, stop and investigate. If versions are equal, bump CFBundleVersion in `tauri.conf.json` so the gate can distinguish builds of the same version.

Expect ~10 pre-existing warnings in `publication.rs` — unrelated.

### Clean-boot bootstrap verification

1. `rm -f ~/Library/Application\ Support/wire-node/.credentials`
2. Launch the rebuilt app.
3. Verify bootstrap log:
   ```
   grep "Bootstrapped .credentials" ~/Library/Application\ Support/wire-node/wire-node.log | tail -1
   ```
4. Verify retry log:
   ```
   grep "Reset credential-failed ingest records" ~/Library/Application\ Support/wire-node/wire-node.log | tail -1
   ```
5. Verify `.credentials` at 0600:
   ```
   stat -f "%Sp %N" ~/Library/Application\ Support/wire-node/.credentials
   ```
6. Verify tier modal fires: look for 4+ unassigned tier rows in the app UI (`web`, `mid`, `extractor`, `stale_remote`). Assign models via MVP text field: e.g., `openrouter|x-ai/grok-4.1-fast` for `web`, `openrouter|minimax/minimax-m2.7` for `stale_remote`, and ask Adam for `mid` / `extractor`.

If any step fails, stop.

### Fresh-install simulation (Cycle 2 Stage 1 B M7)

After clean-boot verification succeeds:

1. Backup: `mv ~/Library/Application\ Support/wire-node ~/Library/Application\ Support/wire-node-pre-fresh-install-$(date +%s)`
2. Launch the app. It should start in pristine state.
3. Verify PyramidFirstRun fires. Enter a known-good OpenRouter key.
4. Verify `.credentials` is created with `OPENROUTER_KEY`.
5. Verify `pyramid_config.json` also has the key (legacy sync).
6. Verify the tier routing modal fires for all 6 unassigned tiers. Assign a model to each.
7. Pick a small test folder (3-10 files). Run folder ingest.
8. Verify builds succeed.
9. Restore backup: `rm -rf ~/Library/Application\ Support/wire-node; mv ~/Library/Application\ Support/wire-node-pre-fresh-install-* ~/Library/Application\ Support/wire-node`.

### 14-item verification checklist

After clean-boot + fresh-install simulation passes, re-run folder ingest on `/Users/adamlevine/AI Project Files/agent-wire-node/`. Verify ALL of:

1. **New slugs build successfully.** `SELECT COUNT(*) FROM pyramid_slugs WHERE created_at > '<retest-start>' AND node_count > 0` — every retest slug non-zero.

2. **Conversation slug recovers** — stray CC-1 slugs from 15:30:24 may or may not be fixable depending on Bug #6 investigation status; at minimum, the credential error stops firing for them.

3. **Stale engine recovers on old pyramids.** Query `pyramid_stale_log` for `agent-wire-node-april9`, `goodnewseveryone-definitive`, `all-docs-definitive` after rebuild — entries show success without credential or tier errors.

4. **DADBEAR silent for new slugs.**
   ```
   grep 'DADBEAR: ingest chain dispatch failed' wire-node.log | awk -F'Z' '$1 > "<retest-start>"' | wc -l
   ```
   Expected: 0.

5. **DADBEAR ingest records in clean terminal states.**
   ```
   sqlite3 pyramid.db "SELECT status, COUNT(*) FROM pyramid_ingest_records WHERE slug IN (<retest slug list>) GROUP BY status;"
   ```
   Expected: no `'pending'` or `'processing'` rows. Code/Document slugs show `'skipped'` (via hoisted guards). Conversation slugs show `'complete'`.

6. **Bug #1 retry + Commit 4 cleanup migration both fired.**
   - Log shows `Reset credential-failed ingest records to pending count=6`.
   - Count rows in `pyramid_ingest_records` with `error_message LIKE '%Phase 0b%'` — expected 0 (deleted by migration).
   - Count rows in `pyramid_dadbear_config` with `content_type IN ('code', 'document')` — expected 0.

7. **Tier routing has all tiers populated** (with values, not NULL).
   ```
   sqlite3 pyramid.db "SELECT tier_name, model_id FROM pyramid_tier_routing WHERE model_id IS NULL;"
   ```
   Expected: 0 rows (Adam assigned all during clean-boot modal).
   
   ```
   sqlite3 pyramid.db "SELECT COUNT(*) FROM pyramid_tier_routing;"
   ```
   Expected: >=6 (the auto-discovered set plus any Adam added).

8. **Cost sanity** — `pyramid-cli cost <newly-built-code-slug>` shows token cost proportional to content size. Not runaway.

9. **Reading-mode walk** — `pyramid-cli walk <newly-built-slug> --limit 5` shows coherent text output.

10. **Settings UI round-trip.** Modify OpenRouter key (append ` x` nonce per Cycle 2 B m10), save, quit app, reopen, verify `.credentials` has the modified value, verify `pyramid_config.json.openrouter_api_key` also updated. Restore.

11. **Bundled ignore patterns take effect.** DB query (not log grep per Cycle 2 Stage 1 A C4):
    ```
    sqlite3 pyramid.db "SELECT source_path FROM pyramid_slugs 
                        WHERE created_at > '<retest-start>' 
                        AND (source_path LIKE '%/.claude/%' 
                             OR source_path LIKE '%/.lab.bak.%' 
                             OR source_path LIKE '%/~/%');"
    ```
    Expected: 0 rows.
    
    Plus:
    ```
    sqlite3 pyramid.db "SELECT COUNT(*) FROM pyramid_chunks WHERE source_path LIKE '%.claude/worktrees/%';"
    ```
    Expected: 0 (Cycle 2 B m1).

12. **No orphaned DADBEAR configs.**
    ```
    sqlite3 pyramid.db "SELECT COUNT(*) FROM pyramid_dadbear_config WHERE slug NOT IN (SELECT slug FROM pyramid_slugs);"
    ```
    Expected: 0.

13. **`.credentials` file has correct perms.**
    ```
    stat -f "%Sp %N" ~/Library/Application\ Support/wire-node/.credentials
    ```
    Expected: `-rw------- .credentials`.

14. **Tier routing modal doesn't re-open** on next boot after assignments are saved. (Validates persistence.)

If any of 1-14 fail, stop and investigate before merging.

### Memory updates

1. **Update `feedback_always_scope_frontend.md`** with 16-orphan-commands framing + dated incident reference.

2. **Write new `feedback_grep_frontend_for_new_ipc.md`** with the generic lesson + Phase 3 incident example.

3. **Update `MEMORY.md` index** with the new feedback file entry.

### PR

Once all 14 items pass:
- Title: `stabilize: tier auto-populate + credential bootstrap + DADBEAR hoist + Settings UI wire + ignore patterns`
- Body: link to this plan, 4 bug writeups + 1 class fix, 5-commit diff, verification run output, full audit history (2 cycles × 4 auditors = 8 audits).

### Then pivot

After merge: filemap format proposal, Phase 1 plan for Self-Describing FS, Bug #6 investigation, Bug #7 fix (5 lines), Bug #8 (partner cached config), 3-way tier routing toggle UX, remaining out-of-scope bugs.

---

## Success Criteria

1. All four bugs (#1, #2, #4) + class fix (#9) have green verifier evidence via the 14-item checklist.
2. No new errors in the log during retest.
3. Old pyramids resume stale checks without errors.
4. Commit structure is 5 focused commits on a single PR.
5. Clean-boot bootstrap verification passes.
6. Fresh-install simulation passes.
7. Settings UI round-trip confirms saves reach the credential store.
8. Tier modal fires for unassigned tiers, user can assign, assignments persist.
9. Memory updates land in the same session.
10. Adam can run folder ingestion on any folder and get real pyramids.

---

## Assumptions (verified post-Cycle-2-Stage-1)

1. `CredentialStore::load_with_bootstrap` is called from main.rs; retry runs in main.rs gated on `BootstrapReport.bootstrapped`. Verified via Cycle 2 C5.
2. `save_atomic` enforces 0600 + parent-dir creation. Verified.
3. `Ok(())` from `fire_ingest_chain` does NOT release claim correctly — that's why Bug #2 uses a hoisted skip instead of patching the arm.
4. No concurrent bootstrap races in main path. Verified.
5. Cleanup via DELETE migration is safe because Part A guards prevent new records; migration is idempotent via `_migration_marker` sentinel.
6. `execute_plan` is idempotent — retest reuses existing slugs.
7. Direct JSON read handles malformed `pyramid_config.json` with WARN.
8. Value validation rejects whitespace/quotes/short keys.
9. `pyramid_test_api_key` reads legacy — known Bug #7, out of scope.
10. Partner LLM config cached — known Bug #8, out of scope.
11. Bundled contribution sync gap — known Bug #25, out of scope, class fix routes around it.
12. `.credentials.fallback` footgun — known Bug #26, Bug #1 guards against it.
13. `save_ingest_record` upsert TOCTOU — known Bug #27, Bug #2's two-layer guard prevents it for Code/Document.
14. `pyramid_tier_routing` schema may need nullable provider_id/model_id — verify at patch time, introduce sentinel if not.
15. `pyramid_list_tier_routing` and `pyramid_save_tier_routing` IPC handlers exist and have expected signatures — verify at patch time.

---

## File Surface for Auditors

Modified:
- `src-tauri/src/pyramid/credentials.rs` — Bug #1 `load_with_bootstrap` + `BootstrapReport`.
- `src-tauri/src/pyramid/dadbear_extend.rs` — Bug #2 hoist + scan-tick guard + delete arm + delete 2 tests.
- `src-tauri/src/pyramid/db.rs` — Bug #2 `mark_ingest_skipped`, Bug #1 `retry_credential_failed_ingest_records`, Bug #9 tier discovery + populator + additive upsert, migration SQL, already-uncommitted ignore patterns.
- `src-tauri/src/pyramid/folder_ingestion.rs` — Bug #2 three emission sites via `maybe_emit_dadbear_config` helper, already-uncommitted tests, Bug #29 doc comment fix, optional Bug #30 skip_dirs unification.
- `src-tauri/src/pyramid/types.rs` — Bug #2 `IngestSkipped` TaggedKind variant.
- `src-tauri/src/pyramid/chain_executor.rs` (or resolve_tier site) — Bug #9 error message upgrade.
- `src-tauri/src/main.rs` — Bug #1 bootstrap wiring + retry, Bug #9 scanner call + populator call.
- `src-tauri/assets/bundled_contributions.json` — already-uncommitted ignore patterns.
- `src/components/PyramidSettings.tsx` — Bug #4.
- `src/components/PyramidFirstRun.tsx` — Bug #4.
- `src/components/PyramidTierRouting.tsx` — NEW, Bug #9 MVP modal.
- `src/components/TierAssignmentBanner.tsx` — NEW, Bug #9.
- `src/components/Settings.tsx` — Bug #9 tab integration.
- `src/hooks/useTierRouting.ts` — NEW, Bug #9 hook.
- `src/App.tsx` (or root) — Bug #9 boot-time modal + banner integration.
- `docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md` — commit untracked.
- Memory files.

Removed in pre-flight:
- `fix_dispatch.patch`
- `~/` repo-root stray tree (408 KB, Adam's confirm).

READ for verification at patch time:
- `src-tauri/src/pyramid/question_build.rs`
- `src-tauri/src/pyramid/stale_engine.rs`
- `src-tauri/src/pyramid/llm.rs`
- `src-tauri/src/pyramid/provider.rs`
- `src-tauri/src/pyramid/stale_helpers_upper.rs` — for Rust tier literal grep.
- `src-tauri/src/pyramid/evidence_answering.rs` — for tier literal grep.
- `src-tauri/src/pyramid/openrouter_webhook.rs` — for tier literal grep.
- `src-tauri/src/main.rs` around 7567, 9780-9830.
- All `chains/defaults/*.yaml` + `chains/prompts/**/*.yaml` for tier names.
- `src-tauri/src/pyramid/wire_migration.rs` — for the boot sequence + where to call tier scanner.
- `src/hooks/useLocalMode.ts` — to match the hook pattern for `useTierRouting`.

---

## Not In This Plan (explicit)

- No changes to Phase 17 folder_ingestion beyond ignore patterns, emission-site refactor, and doc comment fix.
- No rename of `question_build::spawn_question_build`.
- No generic credentials manager UI.
- No 3-way tier routing provider toggle UX (MVP text field only in this commit).
- No fix to `pyramid_test_api_key` (Bug #7).
- No fix to partner cached LlmConfig (Bug #8).
- No `walk_bundled_contributions_manifest` sync fix (Bug #25).
- No `.credentials.fallback` fallback path fix (Bug #26).
- No `save_ingest_record` upsert TOCTOU fix (Bug #27).
- No bundled contribution supersession ever (class fix eliminates the need).
- No wiring of other 15+ orphan IPC commands.
- No security fixes (#10, #11, #13).
- No batch_size refactor (#14).
- No file-size check in ingest_code/ingest_docs (#15).
- No unification of all three ignore systems (partial fix possible via Bug #30; rest deferred).
- No concurrency gating on `spawn_initial_builds` (#17).
- No observability aggregator (#18).
- No pre-flight credential validation (#23).
