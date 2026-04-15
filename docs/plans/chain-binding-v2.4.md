# chain-binding-v2.4 — Recover the Chronological Pipeline + Make Chain Binding Real

> **🚫 SUPERSEDED by `chain-binding-v2.5.md` on 2026-04-07.**
>
> Round 3 of Stage 2 discovery audit found 10+ critical issues that v2.4 introduced via additive simplifications without source reads. v2.5 was written from a full end-to-end source read pass; it grounds every fact in a verified file path + line range.
>
> Do not implement from this file.
>
> ---
>
> **Original status:** v2.4, post Round 2 discovery audit. Supersedes `chain-binding-v2.3.md`.
>
> Lineage: v1 → 4-agent audit → v2.0 → Stage 1 informed audit → v2.1 → MPS audit → v2.2 → Round 1 discovery audit → v2.3 → Round 2 discovery audit → **v2.4**.
>
> **Audit trail:** `chain-binding-v2.4.deltas.md` (this version's deltas), `chain-binding-v2.discovery-corrections.md` (v2.3's deltas). Read the deltas papers for the receipts.
>
> **Single-session shipping convention:** all phases of this plan plus all phases of `recursive-vine-v2-design.md` land in a single session today. No rollback plans, no "users on older binaries" guards.

## Premise

After two discovery audit rounds, three architectural simplifications:

1. **Drop `rust_intrinsic` chain mode.** The `conversation-legacy-chronological` chain id is a magic string the dispatcher recognizes. No new YAML, no new ChainDefinition fields, no Pipeline enum. Two-line dispatcher change.
2. **Add `ContentType::Other(String)` instead of full newtype.** Preserves compile-time exhaustiveness everywhere; opens the value space for new content_types via `Other("transcript.otter")` etc. Old match arms keep compiling unchanged; new sites add `Other(s) => ...` arms.
3. **Drop the migration runner invention.** Use the existing `let _ = conn.execute("ALTER TABLE...")` pattern. Wrap destructive migrations in explicit `BEGIN; ...; COMMIT;` at their site.

These three simplifications collapse most of the cross-phase coupling problems prior versions of the plan kept generating.

Confirmed-true facts the plan still rests on:
- `build_conversation` exists at `build.rs:684+` and is reachable via `vine.rs:571` for vine bunches today, but NOT from the user's main conversation-pyramid create flow (`build_runner.rs:237` routes through `run_decomposed_build`).
- `run_legacy_build` at `build_runner.rs:646-657` already has a working `ContentType::Conversation => build::build_conversation(...)` arm — it's currently unreachable for Conversation but exists.
- FORWARD/REVERSE/COMBINE prompts are inline `pub const` at `build.rs:90,113,135` with confirmed Pillar 37 violation at `:104` (`"Target: 10-15% of input length"`).
- `ContentType::Vine` already exists in `types.rs:31-37` (5 variants) and `'vine'` is in the `pyramid_slugs` CHECK constraint at `db.rs:56`.
- `pyramid_annotations` FK is composite `(slug, node_id)`, both NOT NULL, with `ON DELETE CASCADE` (`db.rs:228-238`).
- `pyramid_slugs` has 3 `AFTER DELETE` triggers at `db.rs:487-505` and accumulated ALTER columns at `:716, :853, :869, :873`.
- `default_chain_id` has a 17-line canonical doc-comment at `chain_registry.rs:79-99` declaring "every content type routes to question-pipeline" as intentional.
- `ChainDefinition` requires `schema_version`, `name`, `version`, `author`, `defaults`, `steps` — adding optional fields requires `#[serde(default)]`.
- `ChainContext.content_type` is already `String` in the executor — Phase 2.5's scope is at the entry-point boundaries, not the executor body.
- No CLI arg parsing exists in `src-tauri/src/main.rs` — `--repair-chains` would need new infrastructure; replaced with a settings panel button.
- No test fixture for "Run 4 reference rebuild" exists in the repo — replaced with a manual smoke check.

## Goals

1. **Stop forward-pass crashes and dead-config drift** (Phase 0).
2. **Make the chronological conversation pipeline reachable from the main user create flow** (Phases 1+2.4).
3. **Make chain selection a per-content-type operator decision** without recompiling Rust (Phase 2.1-2.3).
4. **Open the `ContentType` value space** with `Other(String)` (Phase 2.5).
5. **Persist temporal anchors as first-class data** (Phase 3).
6. **Fix the bootstrap story for both Tier 1 and Tier 2** (Phase 4).

## Non-goals

- Rust-intrinsic chain mode / Pipeline enum / target_function field
- ContentType free-string newtype / capability flag system
- pyramid_schema_version table / migration runner
- supports_staleness flag system / chain-aware stale_engine plumbing
- include_dir + content-hash manifest sync
- --repair-chains CLI flag
- Run-4 reference rebuild test fixture
- Killing the v3 question DSL / parity.rs
- Transcript parser registry / new transcript parsers
- MCP server temporal awareness
- Wire publish content_type contract

---

## Phase 0 — P0 fixes

### 0.1 Fix the UTF-8 panic in `update_accumulators`

**File:** `src-tauri/src/pyramid/chain_executor.rs:6960-6964`

```rust
// max_chars is interpreted as a CHARACTER count here, not bytes.
// Deliberate semantic shift; verified no chain YAML byte-budgets max_chars.
let truncated = new_val
    .char_indices()
    .nth(max_chars)
    .map(|(i, _)| new_val[..i].to_string())
    .unwrap_or(new_val);
```

Sub-tasks:
- `grep -rn 'max_chars' chains/` and confirm no chain treats it as bytes.
- Add unit test: em-dash at `max_chars - 1`, em-dash at `max_chars - 2`, single 4-byte emoji, single 3-byte CJK char. 6 cases.

### 0.2 Delete dead `instruction_map: content_type:` config

**File:** `chains/defaults/question.yaml` (the dead key, exact line found by grep)

Delete the dead `content_type:` instruction_map key. Add a one-line comment pointing at Phase 2 (which subsumes per-content-type prompt routing via the resolver).

### 0.3 Delete `generate_extraction_schema()`

**File:** `src-tauri/src/pyramid/extraction_schema.rs:40`

Verified zero callers in `src-tauri/`. Delete.

### 0.4 Tighten `chunk_transcript` boundary regex

**File:** `src-tauri/src/pyramid/ingest.rs:244-282`

Change the boundary trigger from `line.starts_with("--- ")` to require a label after: e.g., `line.starts_with("--- ") && line.chars().skip(4).next().is_some_and(|c| c.is_uppercase())`. Eliminates stray markdown rule false-positives.

### 0.5 Pillar 37 sweep on `build.rs` prompt constants

**Files:** `src-tauri/src/pyramid/build.rs:90-300` (FORWARD_PROMPT, REVERSE_PROMPT, COMBINE_PROMPT, DISTILL_PROMPT, and any other prompt constants)

Confirmed Pillar 37 violation at `:104`: `"Target: 10-15% of input length."` — direct prescriptive sizing inside the JSON schema description field of `FORWARD_PROMPT`. Likely more violations in the same file.

**Action:**
1. Read all `pub const *_PROMPT` declarations in `build.rs`.
2. Grep each for: `at least`, `between \d`, `minimum`, `maximum`, `at most`, `no more than`, `target:`, `\d+%`, `\d+-\d+`, `exactly N`, `\d+ words`, `\d+ sentences`.
3. Replace each violation with a truth condition:
   - `:104` `"Target: 10-15% of input length."` → `"Compress to maximum density without losing information."`
   - Any `"1-2 sentences"` / `"2-6 word"` violations get replaced with `"as long as needed; no longer"` style truth conditions.
4. Each replacement preserves valid JSON (most are inside `r#"{ "field": "..." }"#` schema descriptions).
5. Tag the commit explicitly: "Pillar 37 sweep — affects vine bunches via shared FORWARD/REVERSE/COMBINE constants."
6. Verification: rebuild a test vine bunch (any existing vine slug) before declaring the phase done. Confirm no regression.

### 0.6 Phase 0 done criteria

- [ ] `update_accumulators` no longer panics on multi-byte UTF-8 input. Test passes.
- [ ] `instruction_map: content_type:` removed from `chains/defaults/question.yaml`.
- [ ] `generate_extraction_schema` deleted.
- [ ] `chunk_transcript` does not false-trigger on stray markdown `---` rules.
- [ ] `build.rs` prompt constants pass Pillar 37 audit.
- [ ] Test vine rebuild passes with the scrubbed prompts.

Ship as 5 small commits.

---

## Phase 1 — Pillar 37, audit, consumer surface (no dispatch fix yet)

Phase 1 contains all the Conversation-pyramid work that doesn't depend on Phase 2's resolver. The dispatch fix moves to Phase 2.4.

### 1.0 Annotations discovery — locate actual deletion mechanism

**File:** various

Before designing an annotations-protection mechanism, find the actual `pyramid_nodes` row-removal sites in the rebuild path. The audits identified:
- `db.rs:2009` — `delete_nodes_above` (marked `#[deprecated]`)
- `parity.rs:545` — unconditional full wipe
- CASCADE from `DELETE FROM pyramid_slugs` at `db.rs:1425, :1536`
- CASCADE from Phase 2.5.3's table-recreate

But the actual rebuild path used by `build_runner.rs` and `vine.rs` does NOT call `delete_nodes_above` (verified by grep — zero hits). It likely uses one of:
- `INSERT OR REPLACE INTO pyramid_nodes` (preserves rows; FK CASCADE doesn't fire because no DELETE)
- `supersede_nodes_above` (the non-deprecated replacement the `#[deprecated]` attribute points at)
- Some other supersession mechanism

**Action:**
1. `grep -rn 'pyramid_nodes' src-tauri/src/pyramid/build_runner.rs src-tauri/src/pyramid/vine.rs src-tauri/src/pyramid/build.rs`
2. Identify the actual write/delete pattern.
3. **If `INSERT OR REPLACE` or supersession (non-DELETE) is used:** annotations are not at risk. Phase 1.0 collapses to "verified, no work needed." Document inline below.
4. **If actual DELETE is used:** add an `archive_annotations_for_slug(conn, slug)` helper and call it BEFORE the delete. Insert into a new `pyramid_orphaned_annotations` table (created via the existing `let _ = CREATE TABLE IF NOT EXISTS` pattern in `init_pyramid_db`).
5. Document the discovery findings inline. Phase 1.0 ships nothing if step 3 is true.

### 1.1 Audit `build_conversation`

**File:** `src-tauri/src/pyramid/build.rs:684+`

- Read end-to-end.
- Confirm L0/L1/L2 node shapes match what the parser accepts.
- Confirm cancellation handling.
- Confirm crash + resume.
- Document any surprises inline below.

### 1.2 Verify FORWARD/REVERSE/COMBINE Pillar 37 (post Phase 0.5)

After Phase 0.5 lands, re-read the three constants in `build.rs:90, :113, :135`. Confirm no prescriptive output sizing remains.

### 1.3 Structural divergence consumer audit

Vine bunches built via `vine.rs:571` already produce question-shape-less pyramids. Inventory their behavior on the consumer surfaces as a free regression baseline, then extend any gaps for the new chronological binding.

**Consumer files (audit table — fill in inline as work proceeds):**

| File | Behavior on chronological pyramid | Gap? | Fix |
|---|---|---|---|
| `src-tauri/src/pyramid/build_runner.rs` (post-build seeding) | TBD | TBD | TBD |
| `src-tauri/src/pyramid/vine.rs::run_build_pipeline` | Already a co-caller | No | Verify in test |
| `src-tauri/src/pyramid/stale_engine.rs` | Silently no-ops (no KEEP links) | Yes | Phase 2.6 |
| `src-tauri/src/pyramid/stale_helpers.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/stale_helpers_upper.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/staleness.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/staleness_bridge.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/publication.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/wire_publish.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/wire_import.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/faq.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/webbing.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/reconciliation.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/partner/` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/public_html/routes_read.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/public_html/routes_ask.rs` | TBD | TBD | TBD |
| `src-tauri/src/pyramid/render.rs` | TBD | TBD | TBD |
| `mcp-server/src/` | TBD | TBD | TBD |

For each: confirm graceful degradation on missing question-shape data, OR fix in this same session.

### 1.4 Phase 1 done criteria

- [ ] Annotations discovery complete; archive helper created if needed (often: not needed).
- [ ] `build_conversation` audited end-to-end; surprises documented.
- [ ] Pillar 37 verified clean for FORWARD/REVERSE/COMBINE post Phase 0.5.
- [ ] Consumer audit table filled in; every breaking surface fixed.

---

## Phase 1F-pre — Frontend constants centralization

Parallel to Phase 1 backend.

1. `grep -rn 'content_type' src/` to find every reference.
2. Centralize in `src/lib/contentTypes.ts`:
   ```typescript
   export const WELL_KNOWN_CONTENT_TYPES = ['code', 'document', 'conversation', 'question', 'vine'] as const;
   export type WellKnownContentType = typeof WELL_KNOWN_CONTENT_TYPES[number];
   export type ContentType = WellKnownContentType | string; // string post-Phase 2.5
   ```
3. Update `AddWorkspace.tsx` and 6+ other call sites to import from this central file.
4. Audit every `CONTENT_TYPE_CONFIG[x]` consumer (`PyramidRow.tsx:30`, `PyramidDetailDrawer.tsx:297`, etc.) and add fallback objects:
   ```typescript
   const config = CONTENT_TYPE_CONFIG[x] ?? { label: x, color: 'gray', icon: 'question-mark' };
   ```

### 1F-pre done criteria

- [ ] Centralized constants file exists.
- [ ] All content_type references in `src/` import from the central file.
- [ ] Every `CONTENT_TYPE_CONFIG[x]` consumer has a fallback object.

---

## Phase 2 — Make chain binding real

### 2.1 Schema change — `pyramid_chain_defaults` table

**File:** `src-tauri/src/pyramid/db.rs` (in `init_pyramid_db`)

Existing pattern (`let _ = conn.execute(...)`) — not the migration runner. Use what's already there:

```rust
let _ = conn.execute(
    "CREATE TABLE IF NOT EXISTS pyramid_chain_defaults (
        content_type TEXT PRIMARY KEY,
        chain_id TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    [],
);
```

Idempotent. Safe.

### 2.2 Add `resolve_chain_for_slug` (override layer over canonical default)

**File:** `src-tauri/src/pyramid/chain_registry.rs`

`default_chain_id` stays. Its 17-line doc-comment is canonical and v2.4 doesn't reverse it. v2.4 adds an OVERRIDE layer.

```rust
/// Resolve the chain ID for a slug build, consulting overrides in this order:
///   1. per-slug assignment (highest priority)
///   2. per-content-type default override
///   3. canonical default (`default_chain_id`)
pub fn resolve_chain_for_slug(
    conn: &Connection,
    slug: &str,
    content_type: &str,
) -> Result<String> {
    if let Some((chain_id, _)) = get_assignment(conn, slug)? {
        return Ok(chain_id);
    }
    if let Some(chain_id) = get_chain_default(conn, content_type)? {
        return Ok(chain_id);
    }
    Ok(default_chain_id(content_type).to_string())
}

pub fn get_chain_default(conn: &Connection, content_type: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT chain_id FROM pyramid_chain_defaults WHERE content_type = ?1",
    )?;
    let result = stmt.query_row(rusqlite::params![content_type], |row| row.get::<_, String>(0));
    match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn set_chain_default(conn: &Connection, content_type: &str, chain_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_chain_defaults (content_type, chain_id) VALUES (?1, ?2)
         ON CONFLICT(content_type) DO UPDATE SET chain_id = excluded.chain_id, updated_at = datetime('now')",
        rusqlite::params![content_type, chain_id],
    )?;
    Ok(())
}
```

**Update the doc-comment on `default_chain_id`** to acknowledge the new override layer (additive note, not a reversal).

**Enumerate `default_chain_id` callers:** `grep -rn 'default_chain_id' src-tauri/src/`. Each caller decides whether to migrate to `resolve_chain_for_slug` (when slug + content_type are in scope) or stay on `default_chain_id`. List the callers and decisions inline below.

### 2.3 IPC commands

Add to `src-tauri/src/main.rs` (or wherever IPC commands live):

```rust
#[tauri::command]
async fn set_chain_default_cmd(content_type: String, chain_id: String, state: State<'_, AppState>) -> Result<(), String> { ... }

#[tauri::command]
async fn get_chain_default_cmd(content_type: String, state: State<'_, AppState>) -> Result<Option<String>, String> { ... }

#[tauri::command]
async fn list_available_chains_cmd(state: State<'_, AppState>) -> Result<Vec<ChainSummary>, String> {
    // Returns hardcoded list: question-pipeline, conversation-legacy-chronological, plus
    // anything in pyramid_chain_assignments. No discovery, no enumeration of YAML files.
}
```

### 2.4 Dispatch fix in `build_runner.rs:237` (the actual code change)

**File:** `src-tauri/src/pyramid/build_runner.rs:237`

Today, the Conversation branch routes to `run_decomposed_build` unconditionally. Fix: consult the resolver.

Pseudocode (real version requires reading the function's locals to thread `write_tx`, `cancel`, etc.):
```rust
// inside run_build_from, at line ~237 for ContentType::Conversation:
let chain_id = chain_registry::resolve_chain_for_slug(
    &conn,
    slug,
    content_type.as_str(),
)?;

if chain_id == "conversation-legacy-chronological" {
    // run_legacy_build at :646-657 already has the working ContentType::Conversation arm.
    // Make it reachable.
    return run_legacy_build(state, slug, /* full args */).await;
}
return run_decomposed_build(state, slug, /* ... */).await;
```

**Sub-tasks:**
- Read `run_build_from`'s locals at the dispatch site. Identify what `run_legacy_build` needs (`write_tx`, `cancel`, `progress_tx`, ...). Thread them.
- Acknowledge the existing `ContentType::Conversation => build::build_conversation(...)` arm at `build_runner.rs:646-657`. It's the actual implementation. The dispatch fix makes it reachable; the arm itself is unchanged.
- No magic-string registration needed. The wizard's `list_available_chains_cmd` returns a hardcoded list including `"conversation-legacy-chronological"` (Phase 2.3).

### 2.5 Open `ContentType` with `Other(String)` variant

**File:** `src-tauri/src/pyramid/types.rs:29-65`

Replace the closed enum with one that has an open escape hatch:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]  // serializes as bare string for both well-known variants and Other
pub enum ContentType {
    #[serde(rename = "code")]
    Code,
    #[serde(rename = "conversation")]
    Conversation,
    #[serde(rename = "document")]
    Document,
    #[serde(rename = "vine")]
    Vine,
    #[serde(rename = "question")]
    Question,
    Other(String),
}
```

Wait — `untagged` with a string-carrying variant might not give the right wire shape. Verify and adjust:

**Alternative (cleaner) — manual Serialize/Deserialize:**
```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContentType {
    Code,
    Conversation,
    Document,
    Vine,
    Question,
    Other(String),
}

impl ContentType {
    pub fn as_str(&self) -> &str {
        match self {
            ContentType::Code => "code",
            ContentType::Conversation => "conversation",
            ContentType::Document => "document",
            ContentType::Vine => "vine",
            ContentType::Question => "question",
            ContentType::Other(s) => s.as_str(),
        }
    }

    /// Strict parser: returns None for unknown strings. For the validation gate at main.rs:3965.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "code" => Some(ContentType::Code),
            "conversation" => Some(ContentType::Conversation),
            "document" => Some(ContentType::Document),
            "vine" => Some(ContentType::Vine),
            "question" => Some(ContentType::Question),
            _ => None,
        }
    }

    /// Open parser: accepts any string. Returns Other(s) for unknown.
    pub fn from_str_open(s: &str) -> Self {
        Self::from_str(s).unwrap_or_else(|| ContentType::Other(s.to_string()))
    }
}

impl serde::Serialize for ContentType {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ContentType {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Ok(ContentType::from_str_open(&s))
    }
}
```

This serializes ALL variants (including `Other(s)`) as a bare string. Wire-compatible with the existing enum. Tauri IPC continues to work. The frontend continues to see `"code"`, `"conversation"`, etc.

**Critical:** keep the strict `from_str` for `main.rs:3965`'s validation gate. Add `from_str_open` for sites that need to accept arbitrary content_types (Phase 2.4 dispatch, Phase 2.5 wizard input, etc.).

### 2.5.1 Update existing match arms with `Other` variant

Most match sites in the 13 files become non-exhaustive after adding `Other`. The compiler will complain. For each:

- **`vine.rs:569-586`**: add `ContentType::Other(_) => Err(anyhow!("vine bunches don't yet support custom content_types"))` arm. Same as the existing `Vine` and `Question` arms.
- **`slug.rs:168-190`**: add `ContentType::Other(_) => { /* no source path validation; let the chain decide */ }` arm.
- **`build_runner.rs:646-657` (`run_legacy_build`)**: add `ContentType::Other(_) => return Err(anyhow!("legacy build doesn't support custom content_types"))` arm.
- **`main.rs:3145-3245` post-build seeding**: add `ContentType::Other(_) => { /* skip file-watcher seeding */ }` arms in each match.
- **All other 9 files**: add `Other` arms with sensible defaults (mostly "skip" or "error").

### 2.5.2 CHECK constraint expansion

`db.rs:56` has `CHECK(content_type IN ('code', 'conversation', 'document', 'vine', 'question'))`. With `ContentType::Other("transcript.otter")` we need to either expand the CHECK or drop it.

**Option A (expand):** add `'other'` to the IN list and serialize `Other(s)` as the string `"other"` in the DB layer (lossy — loses the inner string). Bad.

**Option B (drop CHECK):** table-recreate `pyramid_slugs` without the CHECK. Audit-required ritual:

```rust
fn migrate_drop_pyramid_slugs_check(conn: &Connection) -> Result<()> {
    // 1. Idempotency check: if 'other' is already allowed (no CHECK), skip.
    let check_present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pyramid_slugs' AND sql LIKE '%CHECK%'",
        [],
        |row| row.get(0),
    )?;
    if check_present == 0 { return Ok(()); }

    // 2. Introspect current columns via PRAGMA table_info
    let mut stmt = conn.prepare("PRAGMA table_info(pyramid_slugs)")?;
    let columns: Vec<(String, String, bool, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,        // name
                row.get::<_, String>(2)?,        // type
                row.get::<_, bool>(3)?,          // notnull
                row.get::<_, Option<String>>(4)?, // dflt_value
            ))
        })?
        .collect::<Result<_, _>>()?;

    // 3. Build CREATE TABLE pyramid_slugs_new with all current columns, no CHECK
    let mut create_sql = String::from("CREATE TABLE pyramid_slugs_new (");
    let mut col_names = Vec::new();
    for (i, (name, ty, notnull, dflt)) in columns.iter().enumerate() {
        if i > 0 { create_sql.push_str(", "); }
        create_sql.push_str(&format!("{} {}", name, ty));
        if *notnull { create_sql.push_str(" NOT NULL"); }
        if let Some(d) = dflt { create_sql.push_str(&format!(" DEFAULT {}", d)); }
        if name == "slug" { create_sql.push_str(" PRIMARY KEY"); }
        col_names.push(name.clone());
    }
    create_sql.push(')');

    // 4. Run the migration in a transaction with foreign_keys disabled
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN")?;
    let result: Result<()> = (|| {
        conn.execute(&create_sql, [])?;
        let cols = col_names.join(", ");
        conn.execute(
            &format!("INSERT INTO pyramid_slugs_new ({}) SELECT {} FROM pyramid_slugs", cols, cols),
            [],
        )?;
        conn.execute("DROP TABLE pyramid_slugs", [])?;
        conn.execute("ALTER TABLE pyramid_slugs_new RENAME TO pyramid_slugs", [])?;

        // 5. Recreate the three AFTER DELETE triggers (db.rs:487-505)
        conn.execute_batch(include_str!("recreate_pyramid_slugs_triggers.sql"))?;

        Ok(())
    })();
    match result {
        Ok(()) => { conn.execute_batch("COMMIT; PRAGMA foreign_keys = ON")?; Ok(()) }
        Err(e) => { conn.execute_batch("ROLLBACK; PRAGMA foreign_keys = ON")?; Err(e) }
    }
}
```

**Sub-tasks:**
- Read `db.rs:487-505` and copy the three trigger CREATE statements into a new file `src-tauri/src/pyramid/sql/recreate_pyramid_slugs_triggers.sql`.
- Verify `pyramid_slugs` has no explicit indices (per `grep CREATE.*INDEX.*pyramid_slugs db.rs`); if it does, recreate them too.
- Update `db.rs:54-63` `CREATE TABLE IF NOT EXISTS pyramid_slugs` to remove the CHECK clause as well (so fresh installs don't reintroduce it).
- Idempotency: the migration checks if CHECK is still present; runs only if so. No schema_version table needed.
- Call this migration from `init_pyramid_db` AFTER the existing `CREATE TABLE IF NOT EXISTS pyramid_slugs` line (which on a fresh install creates the new shape; the migration is a no-op; on an existing install creates the old shape, then the migration drops the CHECK).

### 2.6 Simple slug-level chain check at stale_engine entry

**File:** `src-tauri/src/pyramid/stale_engine.rs` (entry point)

Plumbing chain definitions through 5 stale files is too invasive. Simpler:

```rust
// at the top of the propagation entry point:
fn should_skip_stale_propagation(conn: &Connection, slug: &str) -> bool {
    if let Ok(Some((chain_id, _))) = chain_registry::get_assignment(conn, slug) {
        if chain_id == "conversation-legacy-chronological" {
            tracing::warn!(
                "stale propagation skipped for slug {}: chain {} produces no KEEP-link evidence",
                slug, chain_id
            );
            return true;
        }
    }
    false
}
```

Add the check at the entry point of `propagate_staleness` (or whatever the actual entry function is named — find by grep). Same check in `stale_helpers`, `stale_helpers_upper`, `staleness`, `staleness_bridge` if they have their own entry points.

This is hardcoded to the chronological binding only. Future chains that opt out of staleness extend the check. Not generic, but real and ships in this session.

### 2.7 Phase 2 done criteria

- [ ] `pyramid_chain_defaults` table exists.
- [ ] `resolve_chain_for_slug` consults per-slug → per-content-type → canonical fallback.
- [ ] IPC commands `set_chain_default` / `get_chain_default` / `list_available_chains` exposed.
- [ ] Setting `pyramid_chain_defaults[conversation] = 'conversation-legacy-chronological'` routes new conversation builds to `build_conversation` via the dispatch fix.
- [ ] `ContentType::Other(String)` variant added; all match arms updated; `cargo check` clean.
- [ ] `pyramid_slugs` CHECK constraint dropped (migration runs once on existing DBs); 3 triggers recreated; `db.rs:54` updated.
- [ ] `default_chain_id` doc-comment updated to acknowledge override layer.
- [ ] `default_chain_id` callers enumerated and updated.
- [ ] Stale propagation skips for `conversation-legacy-chronological` slugs with logged warning.
- [ ] Test: build a pyramid with content_type `"transcript.test"` (free string), confirm fallback chain runs.
- [ ] Existing slugs assigned to `question-pipeline` continue to build successfully.
- [ ] Existing vine bunches that route through `build_conversation` continue to build successfully.

---

## Phase 1F-post — Frontend wizard wiring

After Phase 2.3 IPC commands ship.

5. Wire the wizard's chain selector to `set_chain_default_cmd` / `get_chain_default_cmd` / `list_available_chains_cmd`.
6. Surface the active chain in the workspace settings panel.
7. Add a settings panel button: "Re-sync bundled chain files (overwrites local edits)" — calls a new IPC `repair_chains_cmd` that runs the bootstrap re-sync (Phase 4). Replaces the v2.3 `--repair-chains` CLI flag.
8. Default the wizard dropdown to well-known content_types; advanced "type custom" toggle behind a settings flag.

### 1F-post done criteria

- [ ] Wizard chain selector persists via IPC.
- [ ] Workspace settings panel shows active chain_id per slug.
- [ ] Settings panel "Re-sync chains" button works.
- [ ] Custom content_type advanced toggle exists.

---

## Phase 3 — Persist temporal anchors as first-class data

### 3.0 Locate the L0 schema parse site(s) — spike

```bash
grep -rn 'serde_json::from_str.*Topic\|from_value.*Topic\|Vec<Topic>' src-tauri/src/pyramid/
grep -rn 'step_outputs.*insert\|store_step_output' src-tauri/src/pyramid/chain_executor.rs
```

Likely multiple sites: extract, extract_with_schema, heal_json retry path. Document inline below.

### 3.1 Add temporal columns to `pyramid_chunks`

**File:** `src-tauri/src/pyramid/db.rs` (in `init_pyramid_db`)

Use the existing `let _ = ALTER TABLE` pattern with PRAGMA pre-check:

```rust
fn add_chunks_temporal_columns_if_missing(conn: &Connection) -> Result<()> {
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(pyramid_chunks)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    if !columns.contains(&"first_ts".to_string()) {
        conn.execute("ALTER TABLE pyramid_chunks ADD COLUMN first_ts TEXT DEFAULT NULL", [])?;
    }
    if !columns.contains(&"last_ts".to_string()) {
        conn.execute("ALTER TABLE pyramid_chunks ADD COLUMN last_ts TEXT DEFAULT NULL", [])?;
    }
    if !columns.contains(&"content_hash".to_string()) {
        conn.execute("ALTER TABLE pyramid_chunks ADD COLUMN content_hash TEXT DEFAULT NULL", [])?;
    }
    Ok(())
}
```

Called from `init_pyramid_db` after the `pyramid_chunks` `CREATE TABLE IF NOT EXISTS`. Idempotent.

### 3.2 Make `Topic.speaker` and `Topic.at` first-class

**File:** `src-tauri/src/pyramid/types.rs:90-108`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    #[serde(default)]
    pub current: String,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub corrections: Vec<Correction>,
    #[serde(default)]
    pub decisions: Vec<Decision>,

    // New: temporal anchors
    #[serde(default)]
    pub speaker: Option<String>,
    #[serde(default)]
    pub at: Option<String>,

    #[serde(flatten, default, deserialize_with = "deserialize_extra_no_temporal")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn deserialize_extra_no_temporal<'de, D>(deserializer: D) -> Result<serde_json::Map<String, serde_json::Value>, D::Error>
where D: serde::Deserializer<'de> {
    let map = serde_json::Map::deserialize(deserializer)?;
    if map.contains_key("speaker") || map.contains_key("at") {
        return Err(serde::de::Error::custom(
            "speaker/at must be top-level Topic fields, not in extra"
        ));
    }
    Ok(map)
}
```

### 3.3 Generic `required_topic_fields` validation in the L0 extract step

**File:** Phase 3.0 spike output (likely chain_executor.rs extract primitive)

1. Add a struct field to `ChainStep` in `chain_engine.rs`:
```rust
#[serde(default)]
pub required_topic_fields: Option<Vec<String>>,
```

2. Verify `defaults_adapter.rs` populates the field on legacy DSL conversion. Default: None (no enforcement).

3. At every parse site identified by Phase 3.0, after deserializing a `Topic`:
```rust
if let Some(required) = &step.required_topic_fields {
    for field in required {
        let value = match field.as_str() {
            "speaker" => topic.speaker.as_deref(),
            "at" => topic.at.as_deref(),
            _ => topic.extra.get(field).and_then(|v| v.as_str()),
        };
        if value.map(|s| s.is_empty()).unwrap_or(true) {
            return Err(...);  // triggers existing parse-retry path
        }
    }
}
```

4. Chain YAML usage:
```yaml
- name: source_extract
  primitive: extract
  instruction: $prompts/question/source_extract.md
  for_each: $chunks
  required_topic_fields:
    - speaker
    - at
```

### 3.4 Re-ingestion idempotency by `content_hash` invalidation

**File:** `src-tauri/src/pyramid/db.rs:1698-1700` (`clear_chunks`) and `src-tauri/src/pyramid/ingest.rs`

On re-ingest:
1. Compute content_hash for each new chunk.
2. For each existing chunk in `pyramid_chunks` at the same chunk_index, compare new hash to old hash. NULL old hash counts as mismatch.
3. If mismatch: invalidate `pyramid_pipeline_steps` for that chunk_index.
4. New chunks with no prior at that index: also invalidate (defensive).

Drop the v2.3 "preserve chunk_index by content_hash matching" alternative — silently corrupts on insertion.

**First post-upgrade re-ingest warning:** every existing chunk has NULL content_hash. The first re-ingest after this phase ships will invalidate every chunk's pipeline_steps in one batch. Big wall-clock cost; the user should expect to rebuild affected slugs once.

### 3.5 Phase 3 done criteria

- [ ] L0 parse site(s) located (3.0 spike output documented).
- [ ] `pyramid_chunks` has `first_ts`, `last_ts`, `content_hash` columns. Populated on ingest.
- [ ] `Topic` has `Option<String>` `speaker` and `at` fields. `extra` deserializer rejects those keys.
- [ ] Chain YAML can declare `required_topic_fields:`; parse site enforces.
- [ ] `defaults_adapter.rs` populates `required_topic_fields` on legacy DSL conversion (default None).
- [ ] Re-ingestion invalidates `pyramid_pipeline_steps` on hash mismatch.
- [ ] An L0 node from a sequential source has populated `speaker`/`at`, verifiable by SQL.

---

## Phase 4 — Bootstrap fixes for both tiers

Simpler than v2.3. No `include_dir!`, no manifest, no compile-time hash. Just fix the two specific bugs.

### 4.1 Tier 1 — change `copy_dir_recursive` to skip overwrite

**File:** `src-tauri/src/pyramid/chain_loader.rs:225-235`

Today `copy_dir_recursive` unconditionally overwrites. Change to skip-if-exists:

```rust
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if !dst_path.exists() {
            std::fs::write(&dst_path, std::fs::read(&src_path)?)?;
        }
        // else: skip — preserve user edits
    }
    Ok(())
}
```

Dev users no longer lose local chain edits on every app start.

### 4.2 Tier 2 — bundle the missing prompt files

**File:** `src-tauri/src/pyramid/chain_loader.rs:225-371`

Verified by audit: Tier 2 only bundles ~12 files. Missing: `chains/prompts/conversation/*`, `chains/prompts/code/*`, `chains/prompts/document/*`, `chains/prompts/conversation-chronological/*`, `chains/prompts/question-conversation/*`.

**Action:** add explicit `include_str!` lines for every missing file. Tedious but correct. List the files to add:

```bash
find chains/prompts -type f -name '*.md' -o -name '*.yaml' | sort
```

For each file in the output, add:
```rust
let conv_chunk_pre = include_str!("../../../chains/prompts/conversation/chunk_pre.md");
ensure_file(&prompts_dir.join("conversation/chunk_pre.md"), conv_chunk_pre)?;
// ... etc.
```

`ensure_file` is the existing `if !path.exists() { write }` helper. No new mechanism needed.

### 4.3 Settings panel "Re-sync" button (Phase 1F-post step 7)

Add an IPC command `repair_chains_cmd` that calls `chain_loader::ensure_default_chains` with a `force_overwrite: true` parameter. The flag changes the `if !path.exists()` check to unconditional overwrite for that one invocation.

```rust
#[tauri::command]
async fn repair_chains_cmd(state: State<'_, AppState>) -> Result<(), String> {
    chain_loader::ensure_default_chains_force(&state.app_data_dir).map_err(|e| e.to_string())
}
```

### 4.4 Malformed YAML — hard error

Today: malformed YAML probably already errors at chain parse time. Verify and document. If silent fallback exists, replace with hard error + log path.

### 4.5 Phase 4 done criteria

- [ ] `copy_dir_recursive` no longer overwrites existing files.
- [ ] Tier 2 bundles every prompt file in `chains/prompts/`.
- [ ] `repair_chains_cmd` IPC command exists and overwrites unconditionally.
- [ ] Malformed YAML is a hard error.

---

## Phase 5 — Documentation

```
docs/chain-development/
├── README.md
├── 01-architecture.md         — content_type → resolver → chain_id → executor
├── 02-chain-yaml-reference.md
├── 03-prompt-anatomy.md
├── 04-temporal-conventions.md — chunks columns, Topic.speaker/at, required_topic_fields
├── 05-pillar-37.md
├── 06-forking-a-chain.md
├── 07-adding-a-content-type.md — using ContentType::Other(String)
├── 08-testing-a-chain.md
└── 09-troubleshooting.md
```

---

## Sequencing (single session)

```
Phase 0   (P0 fixes — ships first)
   0.1 UTF-8 panic
   0.2 dead instruction_map key
   0.3 dead generate_extraction_schema
   0.4 chunk_transcript regex
   0.5 Pillar 37 sweep on build.rs
   │
Phase 1F-pre  (frontend constants — parallel to Phase 1 backend)
   │
Phase 1   (Conversation pyramid foundation, no dispatch fix)
   1.0 annotations discovery
   1.1 build_conversation audit
   1.2 verify Pillar 37 in build.rs
   1.3 consumer audit
   │
Phase 2.1  (chain_defaults table)
Phase 2.2  (resolve_chain_for_slug + doc comment + caller updates)
Phase 2.3  (IPC commands)
Phase 2.5  (ContentType::Other variant + match arm updates + CHECK drop migration)
Phase 2.6  (slug-level stale skip)
   │
Phase 2.4  (dispatch fix in build_runner.rs:237 — uses 2.2 resolver, 2.5 enum variant)
Phase 1F-post  (wizard IPC wiring + settings panel)
   │
Phase 3.0  (L0 parse site spike)
Phase 3.1  (chunks temporal columns)
Phase 3.2  (Topic typed fields)
Phase 3.3  (required_topic_fields enforcement)
Phase 3.4  (re-ingest hash invalidation)
   │
Phase 4   (bootstrap: skip-overwrite + missing-file bundle + repair button)
   │
Phase 5   (docs)
   │
──── recursive-vine-v2 begins (sibling plan, same session) ────
```

## Risks

1. **`build_conversation` audit reveals it's broken in some way the line-of-code reading missed.** Mitigation: the function is reachable via vine.rs today, so any structural breakage would already be visible in vine bunch builds. If vine bunches work, `build_conversation` works.
2. **The `copy_dir_recursive` skip-overwrite change masks future intentional updates.** Mitigation: settings panel "Re-sync" button (Phase 4.3) handles this.
3. **`pyramid_slugs` table-recreate misses an ALTER-added column.** Mitigation: PRAGMA table_info introspection (Phase 2.5.2) catches every column at runtime. Idempotency check skips if already migrated.
4. **`ContentType::Other(_) => Err(...)` arms in vine.rs/run_legacy_build silently disable the chronological binding for vines.** This is intentional — vine bunches use the existing dispatch and don't need the binding. But document inline.

## Done criteria (overall)

- [ ] Phase 0: 5 P0 fixes shipped; vine rebuild verified post Pillar 37 sweep.
- [ ] Phase 1: annotations discovery complete (likely no-op); build_conversation audited; consumer audit table filled.
- [ ] Phase 1F-pre: frontend centralized; fallback objects on every Record consumer.
- [ ] Phase 2: chain_defaults table + IPC + resolver layered over canonical default; ContentType::Other variant + all match arms updated; CHECK constraint dropped via PRAGMA-introspected table-recreate; 3 triggers recreated; stale propagation skips for chronological slugs.
- [ ] Phase 2.4: dispatch fix lands; conversation pyramid bound to chronological chain executes `build_conversation`.
- [ ] Phase 1F-post: wizard chain selector wired; settings panel shows active chain; "Re-sync" button works.
- [ ] Phase 3: L0 parse sites named; chunks temporal columns; Topic typed speaker/at; required_topic_fields enforced; re-ingest hash invalidation.
- [ ] Phase 4: copy_dir_recursive skip-overwrite; Tier 2 bundles all prompts; repair button works; malformed YAML hard errors.
- [ ] Phase 5: doc tree exists.
- [ ] Manual smoke test: build any conversation .jsonl with chronological binding; verify L0 nodes have populated content and apex headline.
- [ ] Existing slugs assigned to `question-pipeline` continue to build successfully.
- [ ] Existing vine bunches that route through `build_conversation` continue to build successfully.
- [ ] `cargo build` clean. Existing `cargo test` suite passes plus new UTF-8 boundary test.
