# recursive-vine-v2 — Phase 2 + Phase 4 Prep

> **Status:** scouting doc, written 2026-04-07. Companion to `recursive-vine-v2.md`. Implementation pending.
>
> **What this is:** the research + integration plan for shipping the two deferred pieces of recursive-vine-v2 (Phase 2 recursive ask escalation; Phase 4 cross-operator vines). Both depend on existing infrastructure that lives in this repo or in the Wire side; this doc maps every hook point and contract change so implementation is mechanical, not exploratory.

---

## Phase 2 — Gap-to-Ask Recursive Escalation

### What it does

When the gap dispatcher in `chain_executor.rs:5307+` runs its evidence resolution and the result is insufficient (low evidence count, MISSING verdicts persist), automatically:

1. Synthesize a child question pyramid scoped to the source pyramid using the gap as the apex question.
2. Build the child pyramid via `run_decomposed_build`.
3. Re-resolve evidence from the now-enriched source pyramid (which has new nodes from the child build).
4. Bound the recursion via depth + accuracy thresholds.

The "vine value loop" — vines grow by *asking source pyramids new questions* — is gated on this phase. Without it, vines surface existing nodes only.

### Verified hook points

These all exist today; Phase 2 wires them together.

- **`build_runner::run_decomposed_build`** at `build_runner.rs:702-924`. Async function that takes `(state, slug_name, apex_question, granularity, max_depth, from_depth, characterization, cancel, progress_tx, layer_tx)` and runs the question pipeline. Returns `Result<(String, i32, Vec<StepActivity>)>`. Already handles cross-slug L0 loading at lines 733-753 when content_type is `'question'` AND referenced_slugs is non-empty.
- **`db::create_slug(conn, slug, &ContentType, source_path)`** at `db.rs:1320-1334`. Creates a `pyramid_slugs` row.
- **`db::save_slug_references(conn, slug, &[String])`** at `db.rs:1591-1602`. Bulk-inserts cross-pyramid refs into `pyramid_slug_references`.
- **`db::save_question_tree(conn, slug, build_id, &tree_json)`** — called by the question pipeline to persist the decomposed tree. Used at `build_runner.rs:241+` (`db::get_question_tree`).
- **`chain_executor.rs:5307+` (the gap dispatcher loop)** — already modified by Phase 1.2 to call `resolve_pyramids_for_gap`. Phase 2's escalation trigger goes inside this loop after resolution returns insufficient evidence.
- **`OperationalConfig::tier3`** — the tier-3 config struct that holds long-cycle parameters. Add 2 new fields: `vine_max_recursion_depth: u32` (default 2), `vine_evidence_threshold: usize` (default 3 nodes).
- **`db::get_question_tree`** + **`db::save_question_tree`** for the child pyramid's tree state.
- **`pyramid_slug_references` table** at `db.rs:702-712` — already records (slug, referenced_slug) pairs. Phase 2 inserts (child_question_slug, source_pyramid_slug) pairs.

### Implementation sketch

#### 2.1 Config: depth + threshold

**File:** wherever `Tier3Config` lives (likely `mod.rs` or `tiers.rs` in `pyramid/`).

```rust
pub struct Tier3Config {
    // ... existing fields ...
    /// recursive-vine-v2 Phase 2: maximum depth a vine's recursive ask can
    /// escalate before stopping. depth=0 means "no escalation, surface only";
    /// depth=2 (default) is one level of "ask source pyramid, then walk its
    /// new nodes."
    #[serde(default = "default_vine_max_recursion_depth")]
    pub vine_max_recursion_depth: u32,
    /// recursive-vine-v2 Phase 2: minimum evidence node count from
    /// resolve_pyramids_for_gap to consider Stage 1 successful. If fewer
    /// than this many nodes match, escalate to Stage 3 (recursive ask).
    #[serde(default = "default_vine_evidence_threshold")]
    pub vine_evidence_threshold: usize,
}
fn default_vine_max_recursion_depth() -> u32 { 2 }
fn default_vine_evidence_threshold() -> usize { 3 }
```

#### 2.2 Recursion tracking

The recursion depth needs to thread through the gap dispatcher. The current dispatcher doesn't have a recursion counter — Phase 2 adds one.

Two options:
- **(A)** Add `current_recursion_depth: u32` to a new field on `chain_executor::ChainContext` or thread it through the gap-processing function signature.
- **(B)** Store it in a per-slug DB column (`pyramid_slugs.vine_recursion_depth`) that gets bumped on entry and decremented on completion. Persistent across crashes.

**Recommendation:** option (A). Per-slug DB column is overkill for what is essentially a runtime counter that resets on every top-level build.

#### 2.3 Trigger condition in the gap dispatcher

**File:** `chain_executor.rs:5307-5455` (the existing gap loop, already partially modified by Phase 1.2)

After `resolve_files_for_gap` + `resolve_pyramids_for_gap` complete and `combined` evidence is collected:

```rust
let evidence_count = combined.len();
let pyramid_count = combined.iter().filter(|(slug, _, _)| pyramid_slugs.contains(slug)).count();

// Phase 2 (recursive-vine-v2): if pyramid evidence is below threshold AND
// recursion depth allows, escalate to ask the source pyramids new questions.
let max_depth = state.operational.tier3.vine_max_recursion_depth;
let threshold = state.operational.tier3.vine_evidence_threshold;
let can_escalate = current_recursion_depth < max_depth
    && !pyramid_slugs.is_empty()
    && pyramid_count < threshold;

if can_escalate {
    info!(
        slug,
        gap_id = %gap.question_id,
        pyramid_count,
        threshold,
        depth = current_recursion_depth,
        "vine: escalating to recursive ask on source pyramids"
    );
    let escalation_results = escalate_gap_to_source_pyramids(
        state,
        slug,
        &gap,
        &question_text,
        &pyramid_slugs,
        current_recursion_depth + 1,
        &llm_config,
        cancel,
    ).await;
    // Re-run resolve_pyramids_for_gap on the now-enriched sources
    // ...
}
```

#### 2.4 The escalation function

New function in `chain_executor.rs` or `evidence_answering.rs`:

```rust
/// recursive-vine-v2 Phase 2: spawn a child question pyramid on each source
/// pyramid with the gap as the apex question. Builds via run_decomposed_build,
/// which enriches the source pyramid with new answer nodes. Returns the count
/// of source pyramids successfully escalated.
pub async fn escalate_gap_to_source_pyramids(
    state: &PyramidState,
    parent_slug: &str,
    gap: &GapReport,
    question_text: &str,
    source_pyramid_slugs: &[String],
    recursion_depth: u32,
    llm_config: &LlmConfig,
    cancel: &CancellationToken,
) -> Result<usize> {
    let mut escalated = 0;
    for source_slug in source_pyramid_slugs {
        // Build a child question slug name. Hash the gap so re-asks are stable.
        let gap_hash = format!("{:x}", sha2::Sha256::digest(gap.description.as_bytes()))[..8].to_string();
        let child_slug = format!("{}--ask-{}", source_slug, gap_hash);

        // Create the child slug if it doesn't exist
        {
            let conn = state.writer.lock().await;
            if db::get_slug(&conn, &child_slug)?.is_none() {
                db::create_slug(&conn, &child_slug, &ContentType::Question, "")?;
                db::save_slug_references(&conn, &child_slug, &[source_slug.clone()])?;
                info!(child_slug = %child_slug, source = %source_slug, "vine: created child question pyramid");
            }
        }

        // Build the child pyramid recursively. The child's gap dispatcher will
        // also see recursion_depth and stop at max_depth.
        let build_result = Box::pin(build_runner::run_decomposed_build(
            state,
            &child_slug,
            question_text, // gap's source question becomes the child's apex
            3,             // granularity default
            2,             // shallow tree for sub-questions
            0,             // from_depth
            None,          // characterization
            cancel,
            None,          // progress_tx
            None,          // layer_tx
        )).await;

        match build_result {
            Ok((build_id, node_count, _)) => {
                info!(child_slug, build_id, node_count, "vine: child pyramid built");
                escalated += 1;
            }
            Err(e) => {
                warn!(child_slug, error = %e, "vine: child pyramid build failed");
            }
        }
    }
    Ok(escalated)
}
```

#### 2.5 Recursion-depth threading

The newly-built child pyramid will hit its OWN gap dispatcher loop. It needs to know the current recursion depth so it doesn't loop forever.

Two options:
- **(A)** Pass recursion_depth via an `OperationalConfig` override that the child build sees.
- **(B)** Add a recursion_depth column to `pyramid_slugs` (per option B above).

**Recommendation:** add `recursion_depth: u32` to the `ChainContext` initial_params (which is already passed through `run_decomposed_build` → `execute_chain_from`). The gap dispatcher reads it from `ctx.initial_params.get("recursion_depth")` with default 0.

#### 2.6 Re-resolve after escalation

After `escalate_gap_to_source_pyramids` returns, re-run `resolve_pyramids_for_gap` against the same source slugs. The source pyramids now have new question-pyramid children with answer nodes that reference them. The evidence resolver picks them up on the next pass.

#### 2.7 Phase 2 done criteria

- [ ] `Tier3Config.vine_max_recursion_depth` + `vine_evidence_threshold` fields exist with sensible defaults
- [ ] `escalate_gap_to_source_pyramids` function exists in `evidence_answering.rs` or `chain_executor.rs`
- [ ] Gap dispatcher loop in `chain_executor.rs:5307+` calls escalate when below threshold and depth allows
- [ ] Recursion depth threaded via `ChainContext.initial_params`
- [ ] Child question pyramids created with correct slug naming + reference linking
- [ ] After escalation, evidence is re-resolved from the enriched sources
- [ ] Manual smoke test: vine pyramid → gap → escalation → child build → re-resolved evidence has more nodes than pre-escalation
- [ ] No infinite recursion: bound check fires at depth N
- [ ] Existing question pyramids unaffected (escalation only fires for slugs with pyramid sources)

---

## Phase 4 — Cross-Operator Vines

### What it does

Allows a vine to declare sources that are *published pyramids on other operators' Wire Nodes*, not just local slugs. Three sub-pieces:

1. **Remote pyramid source refs.** Extend the slug-references model to record remote handle paths (`wire://operator-foo/some-pyramid`) alongside local slug names.
2. **Remote evidence resolution.** When the vine evidence dispatcher encounters a remote source, fetch nodes via `RemotePyramidClient` instead of local `db::get_all_live_nodes`.
3. **Access tier enforcement.** Local evidence walker should respect `pyramid_slugs.access_tier` (defense in depth; the Wire server is the network-level authority).
4. **Credit flow.** Vine evidence queries against remote pyramids should flow through the existing `pyramid_unredeemed_tokens` retry queue and the Wire server's `redeem_token` endpoint. The Wire server side needs one string addition to accept `query_type: "vine_evidence"`.

### Verified hook points

- **`pyramid_remote_web_edges` table** at `db.rs:940+` — already records remote handle paths + tunnel URLs for web-edge connections. The schema is the template for vine remote source refs.
- **`RemotePyramidClient::remote_drill`** in `wire_import.rs` — fetches a single node from a remote pyramid via the `/drill/:slug/:node_id` endpoint of the remote operator's Wire Node. Already used by `build_runner.rs:400+ resolve_remote_web_edges`.
- **`RemotePyramidClient::new(tunnel_url, jwt, wire_server_url)`** — constructor.
- **`pyramid_unredeemed_tokens` table** at `db.rs:976-1000` — payment retry queue for query operations.
- **`pyramid_slugs.access_tier` column** at `db.rs:885` — `'public'` / `'circle-scoped'` / `'priced'` / `'embargoed'`.
- **`pyramid_slugs.access_price`** + **`allowed_circles`** — enforcement metadata.
- **`HandlePath::parse(remote_handle_path)`** — parses `slug/depth/node_id` triples. Used at `build_runner.rs:441`.
- **Wire side (`GoodNewsEveryone`)**: existing `redeem_token` endpoint accepts `query_type` strings. The contract change is adding `"vine_evidence"` to the recognized list (single match arm or string set).

### Implementation sketch

#### 4.1 Schema: remote pyramid source refs

**File:** `db.rs::init_pyramid_db`

Two options:
- **(A)** Extend `pyramid_slug_references` with optional `remote_handle_path` and `remote_tunnel_url` columns. NULL = local ref; NOT NULL = remote ref.
- **(B)** New sibling table `pyramid_remote_slug_references` mirroring `pyramid_remote_web_edges`.

**Recommendation:** option (B). Cleaner separation, mirrors the existing `pyramid_remote_web_edges` pattern, doesn't risk breaking the local-only resolver paths.

```sql
CREATE TABLE IF NOT EXISTS pyramid_remote_slug_references (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
    remote_handle_path TEXT NOT NULL,
    remote_tunnel_url TEXT NOT NULL,
    reference_type TEXT NOT NULL DEFAULT 'vine-source',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(slug, remote_handle_path)
);
CREATE INDEX IF NOT EXISTS idx_remote_slug_refs_slug ON pyramid_remote_slug_references(slug);
```

Helpers:
- `db::save_remote_slug_reference(conn, slug, remote_handle_path, remote_tunnel_url)`
- `db::get_remote_slug_references(conn, slug) -> Vec<RemoteSlugRef>`

#### 4.2 Remote evidence resolution

**File:** `evidence_answering.rs`, alongside `resolve_pyramids_for_gap`

```rust
/// recursive-vine-v2 Phase 4: resolve evidence from REMOTE pyramid sources
/// (other operators' published pyramids) by fetching nodes via
/// RemotePyramidClient. Mirrors `resolve_pyramids_for_gap`'s shape so the
/// dispatcher can swap freely. Includes access-tier enforcement.
pub async fn resolve_remote_pyramids_for_gap(
    state: &PyramidState,
    remote_refs: &[RemoteSlugRef],
    gap_description: &str,
    max_nodes: usize,
    wire_jwt: &str,
    wire_server_url: &str,
) -> Result<Vec<(String, String, String)>> {
    let keywords: Vec<String> = gap_description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(String::from)
        .collect();
    if keywords.is_empty() { return Ok(Vec::new()); }

    let mut clients: HashMap<String, RemotePyramidClient> = HashMap::new();
    let mut scored: Vec<(String, String, String, usize)> = Vec::new();

    for rref in remote_refs {
        let client = clients.entry(rref.remote_tunnel_url.clone()).or_insert_with(|| {
            RemotePyramidClient::new(
                rref.remote_tunnel_url.clone(),
                wire_jwt.to_string(),
                wire_server_url.to_string(),
            )
        });

        // Parse the handle path
        let handle = match HandlePath::parse(&rref.remote_handle_path) {
            Some(h) => h,
            None => continue,
        };

        // Fetch the apex/L1 nodes via remote_search (or remote_drill on the apex)
        // The remote operator's Wire Node enforces access tier on its end.
        // Local enforcement is defense in depth (see 4.3).
        match client.remote_search(&handle.slug, gap_description, max_nodes).await {
            Ok(search_results) => {
                for hit in search_results {
                    let pseudo_path = format!("wire://{}/{}/{}",
                        rref.remote_tunnel_url, handle.slug, hit.node_id);
                    let body = format!(
                        "## REMOTE SOURCE NODE {}\n\n## HEADLINE\n{}\n\n## DISTILLED\n{}",
                        pseudo_path, hit.headline, hit.snippet
                    );
                    let score = keywords.iter()
                        .filter(|kw| body.to_lowercase().contains(kw.as_str()))
                        .count();
                    if score > 0 {
                        scored.push((rref.remote_handle_path.clone(), pseudo_path, body, score));
                    }
                }
            }
            Err(e) => {
                warn!(handle = %rref.remote_handle_path, error = %e,
                      "vine: remote pyramid query failed");
            }
        }
    }

    scored.sort_by(|a, b| b.3.cmp(&a.3));
    scored.truncate(max_nodes);
    Ok(scored.into_iter().map(|(slug, path, content, _)| (slug, path, content)).collect())
}
```

`RemotePyramidClient` already has `remote_drill`. May need `remote_search` — check if it exists or add it as a sibling.

#### 4.3 Access tier enforcement (local defense in depth)

**File:** `evidence_answering.rs::resolve_pyramids_for_gap`

```rust
for source_slug in pyramid_source_slugs {
    // Phase 4.3 (recursive-vine-v2): defense-in-depth access tier check.
    // The Wire server enforces network-level access; local evidence walker
    // enforces local access (e.g., circle-scoped slugs only readable by
    // operators in the circle).
    if let Ok(Some(slug_info)) = db::get_slug(conn, source_slug) {
        if let Ok((tier, price, circles)) = db::get_access_tier(conn, source_slug) {
            if !is_querier_allowed(querier_op_id, &tier, &circles) {
                warn!(slug = %source_slug, tier = %tier,
                      "vine: querier not allowed by access tier, skipping");
                continue;
            }
        }
    }
    // ... existing logic ...
}
```

Helper: `is_querier_allowed(querier_op_id, tier, circles) -> bool` — public always true, circle-scoped checks membership, priced/embargoed always require token-based access (handled separately by 4.4).

#### 4.4 Credit flow integration

**File:** `evidence_answering.rs::resolve_remote_pyramids_for_gap`

When making a remote query against a paid pyramid (`access_tier = 'priced'`), the call needs to mint a payment token via the Wire server, attach it to the request, and on response insert into `pyramid_unredeemed_tokens` for retry tracking.

Existing infrastructure:
- `pyramid_unredeemed_tokens` table at `db.rs:976-1000` already has the columns (nonce, payment_token, querier_operator_id, slug, query_type, stamp_amount, access_amount, total_amount, etc.)
- Wire server's `/redeem_token` endpoint already exists (used by other paid query paths)

The change for vine queries:
1. Before the `remote_search` call, mint a token via `wire_client.mint_token(slug, "vine_evidence", stamp_amount, access_amount).await`.
2. Pass the token in the `remote_search` request headers.
3. On success, insert into `pyramid_unredeemed_tokens` with `query_type = "vine_evidence"`.
4. The existing retry queue handles eventual redemption against the Wire server.

**Wire server change (cross-repo, GoodNewsEveryone):** the `/redeem_token` endpoint's `query_type` accepted-values list needs to include `"vine_evidence"`. This is a one-line addition in the Wire server's payment validator. Track it as a separate ticket on the GoodNewsEveryone repo: ~~"add `vine_evidence` to redeem_token query_type allowlist"~~.

#### 4.5 Dispatcher integration

**File:** `chain_executor.rs:5307+` (the existing gap dispatcher loop modified by Phase 1.2)

Add a third resolution branch for remote pyramid sources:

```rust
let local_pyramid_slugs: Vec<String> = /* existing partition */;
let remote_refs: Vec<RemoteSlugRef> = db::get_remote_slug_references(conn, slug)?;

// Existing file resolution
let mut combined = resolve_files_for_gap(&conn, &file_slugs, &gap_desc, &[], max_files)?;

// Phase 1.2: local pyramid resolution
combined.extend(resolve_pyramids_for_gap(&conn, &local_pyramid_slugs, &gap_desc, max_files)?);

// Phase 4: remote pyramid resolution
if !remote_refs.is_empty() {
    let wire_jwt = state.config.read().await.auth_token.clone();
    let wire_server_url = std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());
    let remote_results = resolve_remote_pyramids_for_gap(
        state, &remote_refs, &gap_desc, max_files, &wire_jwt, &wire_server_url,
    ).await?;
    combined.extend(remote_results);
}
```

#### 4.6 Wizard / IPC for declaring remote sources

A new IPC command `pyramid_add_remote_source` that takes `(slug, remote_handle_path, remote_tunnel_url)` and inserts into `pyramid_remote_slug_references`. The wizard "Domain Vine" UI extension (still a follow-up from Phase 3.1) gains a "Add remote pyramid" option that calls this IPC.

#### 4.7 Phase 4 done criteria

- [ ] `pyramid_remote_slug_references` table exists with helpers
- [ ] `resolve_remote_pyramids_for_gap` function in `evidence_answering.rs`
- [ ] Gap dispatcher in `chain_executor.rs:5307+` calls all three resolvers (file / local pyramid / remote pyramid)
- [ ] Local access tier check in `resolve_pyramids_for_gap` (defense in depth)
- [ ] `pyramid_add_remote_source` IPC command
- [ ] Credit flow: `pyramid_unredeemed_tokens` rows inserted for paid remote queries with `query_type = "vine_evidence"`
- [ ] Manual smoke test: create a vine that references a published pyramid on a different operator's Wire Node; trigger a build with a gap; verify remote nodes are fetched + scored + included as evidence
- [ ] **Cross-repo follow-up filed:** "Wire server: accept `vine_evidence` in `/redeem_token` query_type allowlist"

---

## Sequencing

```
Phase 2 — recursive ask escalation
   2.1 Tier3Config fields
   2.2 ChainContext recursion depth
   2.3 Trigger condition in gap dispatcher
   2.4 escalate_gap_to_source_pyramids function
   2.5 Recursion-depth threading
   2.6 Re-resolve after escalation
   │
Phase 4 — cross-operator vines (local pieces)
   4.1 pyramid_remote_slug_references table + helpers
   4.2 resolve_remote_pyramids_for_gap
   4.3 Access tier enforcement
   4.4 Credit flow integration
   4.5 Dispatcher integration
   4.6 IPC for adding remote sources
   │
Cross-repo: Wire server query_type allowlist (separate ticket)
```

Phase 2 ships first (lower complexity, no cross-repo). Phase 4 local pieces ship second. Cross-repo contract change is its own ticket.

## Risks

1. **Phase 2 infinite recursion.** If recursion-depth threading is buggy, a vine that asks a vine that asks a vine... could fan out unbounded. Mitigation: hard depth bound + bounds-check at every escalation entry. Test with `vine_max_recursion_depth = 0` to verify the bound fires.
2. **Phase 2 child pyramid name collisions.** Stable hashing of the gap question into the slug name avoids this; a re-build with the same gap reuses the existing child slug rather than spawning a new one each time.
3. **Phase 4 remote query latency.** Remote pyramids may be slow or offline. Mitigation: timeout per remote call (already in `RemotePyramidClient`); failed remote queries fall back to whatever local evidence exists.
4. **Phase 4 access tier spoofing.** Local enforcement is defense in depth; the Wire server is the authority. If a local check is wrong but the Wire server is correct, no actual leak occurs.
5. **Phase 4 credit flow double-charging.** If a vine query retries due to a remote failure, the same token shouldn't be redeemed twice. Mitigation: the `pyramid_unredeemed_tokens` table already has unique constraints on `nonce` to prevent this.

## Audit cycle plan

Same pattern as chain-binding-v2.5:

1. **Stage 0 (this doc):** scout the implementation paths. Done.
2. **Stage 1 informed audit:** two auditors review this doc + the verified hook points + the existing Phase 1 implementation. Goal: catch unverified claims, find missing match arms, find existing infrastructure I missed.
3. **Apply Stage 1 findings to a v2 prep doc.**
4. **Stage 2 discovery audit:** two auditors blind to Stage 1, verifying every claim against source.
5. **Apply Stage 2 findings.**
6. **Implementation pass.** Cargo check at every phase boundary. Same discipline as chain-binding-v2.5.
7. **Post-mortem.**

The auditors will be launched in parallel after this doc lands. Their reports go into `/tmp/phase-2-4-audit-*.md` files for the user to inspect.
