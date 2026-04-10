# Wire Discovery and Ranking Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Wire contribution mapping (for the metadata schema + publish flow), config contribution & Wire sharing (for pull flow and supersession)
**Unblocks:** Contribution marketplace, auto-update of pulled configs, quality signals surfaced in search, recommendation UI
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

Basic Wire search (by `schema_type` + tags + free text) is the minimum viable discovery. It's not enough for a functional marketplace. Users need ranking, recommendations, quality signals, and notifications when better versions of contributions they've already pulled become available.

This spec defines:

- A **ranking layer** over raw Wire search results with a composite score built from multiple quality signals
- A **recommendations engine** that suggests configs based on similarity to the user's pyramids
- A **notification system** for superseded configs the user has pulled
- **Quality badges** surfaced alongside search results

The ranking algorithm is itself a contribution. Its weights live in a `wire_discovery_weights` template contribution, so the ranking algorithm is subject to the same improvement system as every other behavior in Wire Node.

---

## Problem Statement

The `pyramid_search_wire_configs` endpoint (defined in `config-contribution-and-wire-sharing.md`) returns flat results ordered by Wire's native relevance. That's fine for bootstrap but falls short in several ways:

- **No quality signal**: a 5-star config with 200 adopters ranks next to a 1-star config with 0 adopters
- **No freshness signal**: abandoned 2023 configs rank next to actively maintained 2026 configs
- **No recommendation**: a user building a code pyramid with OpenRouter has to manually hunt for configs that fit
- **No notification**: users pulling a config have no way to know when a better version appears
- **No quality badges**: search results are text-only with no at-a-glance signal quality

This spec addresses all five gaps.

---

## Ranking Signals

Each Wire contribution returned from search carries metadata that feeds the ranking algorithm. Signals are normalized to `[0, 1]` before weighting.

| Signal | Source | Normalization | Rationale |
|---|---|---|---|
| **Rating** | Wire's native rating system (1-5 stars) | `rating / 5.0` | User-reported quality — the strongest quality signal when it exists |
| **Adoption count** | Count of distinct nodes that pulled this contribution | `log(1 + adoption) / log(1 + max_adoption_in_result_set)` | Log-scaled to prevent "popular gets more popular" runaway |
| **Freshness** | Days since last supersession or update | `max(0, 1 - days_since_update / 180)` | Linear decay over 180 days; reflects active maintenance |
| **Chain length** | Length of the supersession chain rooted at this contribution | `min(chain_length / 10, 1.0)` | Longer chains mean more refinement rounds — proxy for maturity |
| **Author reputation** | Wire's native reputation score for the author | `reputation / max_reputation` | Known-good authors surface higher |
| **Challenge success rate** | Rebuttals upheld vs. total rebuttals filed | `1 - (upheld_rebuttals / (filed_rebuttals + 1))` | Contributions with successful challenges rank lower |
| **Internalization rate** | Pullers who kept the contribution active vs. reverted/deleted | `kept / max(1, total_pullers)` | Behavioral signal — did users actually use it? |

### Composite score

```
score = w_rating * rating_norm
      + w_adoption * adoption_norm
      + w_freshness * freshness_norm
      + w_chain * chain_norm
      + w_reputation * reputation_norm
      + w_challenge * challenge_norm
      + w_internalization * internalization_norm
```

The weights `w_*` sum to 1.0 by convention but the algorithm normalizes if they don't.

### Weight configuration is itself a contribution

The weights live in a `wire_discovery_weights` template contribution:

```yaml
schema_type: wire_discovery_weights
fields:
  w_rating: 0.25
  w_adoption: 0.20
  w_freshness: 0.15
  w_chain: 0.10
  w_reputation: 0.10
  w_challenge: 0.10
  w_internalization: 0.10
```

This follows the foundational rule: every numeric constant in the system is an editable contribution. The user can refine the weights via notes ("rank freshness higher — I'm tired of stale configs"), the LLM regenerates the weights YAML, the user accepts, and the ranking algorithm immediately uses the new weights. The weights contribution can also be Wire-published — users can pull "conservative ranking" or "adoption-first ranking" weight sets from other users.

### Default seed weights

The initial seed weights shown above are tuned for "trust quality signals, penalize staleness, but don't let network effects dominate". They're a starting point — not an absolute standard.

### Missing signals

Not every contribution has every signal. A brand-new contribution has no adoption, no challenges, no internalization data. Missing signals are treated as **neutral**: they contribute zero to the weighted sum, but the corresponding weight is **redistributed** across present signals so new contributions don't automatically rank at the bottom. This gives newcomers a fair shot at being discovered.

---

## Recommendations

Recommendations answer: "for this pyramid, which configs should I consider pulling from Wire?" The engine computes similarity between the user's pyramid and other pyramids on Wire, then surfaces configs those similar pyramids use.

### Similarity signals

| Signal | Source | Weight |
|---|---|---|
| **Source type overlap** | Code vs. document vs. conversation vs. mixed | Strong — code pyramids should see code-tuned configs first |
| **Tier routing similarity** | Which providers and models the pyramid uses | Medium — a local-Ollama pyramid should see local-tuned configs |
| **Schema type match** | The schema_type the user is browsing | Required — recommendations are always filtered to the current schema_type |
| **Apex description similarity** | Embedding similarity of apex text | Deferred to v2 — requires embedding pipeline |

### Recommendation pipeline

```
User opens Discover tab for schema_type "dadbear_policy" while viewing pyramid "my-code-pyramid"
  -> Backend collects pyramid profile: source_type=code, tier_routing=openrouter-mercury, ...
  -> Query Wire for pyramids matching: source_type=code, similar tier_routing
  -> For each matching pyramid, fetch its active dadbear_policy contribution
  -> Aggregate: most common contributions across similar pyramids
  -> Return top-N contributions with a rationale string
```

### Rationale strings

Every recommendation comes with a human-readable explanation of WHY it was recommended:

- "Used by 3 code pyramids with similar tier routing"
- "Top-rated dadbear policy for code pyramids using local models"
- "Pulled by 18 users with folder ingestion heuristics matching yours"

Rationales are composed from the signals that drove the recommendation, so users understand the logic and can decide whether it applies.

### V1 scope

V1 ships with **source type overlap** and **tier routing similarity** only. Apex embedding similarity and other deep semantic signals are deferred to v2. The engine is built to accept new signal functions without changing the pipeline shape, so v2 adds similarity calculators, not new plumbing.

---

## Notifications for Superseded Configs

When a config the user has pulled gets superseded on Wire (the author publishes a new version), the user should know.

### Detection

Wire Node periodically calls the Wire's supersession-check endpoint with the list of `wire_contribution_id` values it has pulled. The endpoint returns, for each ID, whether a newer version exists and what the newer ID is.

```
POST wire/check_supersessions
  Input: { contribution_ids: [wire_contribution_id] }
  Output: [{
    original_id: String,
    latest_id: String,
    chain_length_delta: u32,
    version_labels_between: [String],   # e.g., ["v2: tighten intervals", "v3: add demand signal"]
  }]
```

Wire Node caches responses and polls on a conservative interval (default: every 6 hours, configurable via a `wire_update_polling` template contribution — again, no hardcoded numbers).

### Update badge

When an update is available, the corresponding contribution in ToolsMode → My Tools shows an "Update available" badge. Clicking the badge opens a drawer showing:

- The current version you're on (summary + triggering_note)
- The new version (summary + triggering_note)
- Any intermediate versions (if the chain jumped more than one step)
- The author(s) of each transition
- Changes summary (from the notes)

### Pull latest button

The drawer has a "Pull latest" button that:

1. Pulls the latest Wire contribution into `pyramid_config_contributions`
2. Marks the new version as active (supersedes the currently active version)
3. Triggers `sync_config_to_operational` so runtime tables update immediately
4. The old version remains in the version chain as superseded, preserving history

### Auto-update toggle

For trusted categories, users can opt in to automatic updates. A per-`schema_type` toggle in Settings:

```yaml
auto_update:
  wire_discovery_weights: true
  evidence_policy: false
  dadbear_policy: false
  tier_routing: false
  custom_prompts: true
  folder_ingestion_heuristics: true
```

The default for all categories is `false` — users opt in per category. When enabled for a category, the supersession polling automatically pulls and activates new versions without prompting, logging each transition as `source: "auto-update"` with a triggering_note of "Auto-updated from Wire (chain_length_delta: N)".

Safety net: auto-update is refused for any contribution that would introduce a new credential reference (e.g., a new `${VAR_NAME}` that isn't already defined). The UI surfaces these as a pending manual review.

---

## Quality Signals in Search Results

Search result entries surface quality signals as inline badges so users can scan quickly:

- **⭐ Average rating** — e.g., ⭐ 4.7 (from Wire's native rating)
- **👥 Adoption count** — e.g., 👥 218 (total distinct nodes that pulled it)
- **🚨 Open rebuttals** — shown only if `open_rebuttals > 0`, e.g., 🚨 2 — links to the rebuttal details
- **♻ Chain length** — e.g., ♻ 7 (supersession chain depth; higher = more refined)
- **🆕 Freshness** — "Updated 3d ago" or "Updated 2mo ago" (plain text next to badges)

The badges are rendered by a shared `QualityBadges` React component so they're consistent across the search, recommendations, and update drawer views.

### Badge emoji policy

The emojis above appear in the UI as icon glyphs, not copied from this spec into rendered code. The React components use their own icon set (lucide or heroicons); the emojis here are shorthand for the semantic meanings. The frontend spec will specify exact icon choices — this spec covers intent only.

---

## IPC Contract

```
# Discovery (ranking-enhanced search)
POST pyramid_wire_discover
  Input: {
    schema_type: String,
    query?: String,
    tags?: [String],
    limit?: u32,             # default 20
    sort_by?: String,        # "score" (default) | "rating" | "adoption" | "fresh" | "chain_length"
  }
  Output: [{
    wire_contribution_id: String,
    title: String,
    description: String,
    tags: [String],
    author_handle: String,
    rating: f32,
    adoption_count: u64,
    open_rebuttals: u32,
    chain_length: u32,
    freshness_days: u32,
    score: f32,              # computed composite score
    rationale?: String,      # present when the score was boosted/penalized for an explainable reason
  }]

# Recommendations (for a specific pyramid)
POST pyramid_wire_recommendations
  Input: {
    slug: String,
    schema_type: String,
    limit?: u32,             # default 5
  }
  Output: [{
    wire_contribution_id: String,
    title: String,
    description: String,
    rationale: String,       # "Used by 3 code pyramids with similar tier routing"
    score: f32,
  }]

# Update notification
GET pyramid_wire_update_available
  Input: { slug?: String }   # if omitted, check all contributions
  Output: [{
    local_contribution_id: String,
    schema_type: String,
    slug?: String,
    latest_wire_contribution_id: String,
    version_delta: String,   # human-readable: "3 versions ahead"
    chain_length_delta: u32,
    changes_summary: String, # concatenation of intermediate triggering_notes
    author_handles: [String],
  }]

# Auto-update toggle
POST pyramid_wire_auto_update_toggle
  Input: { schema_type: String, enabled: bool }
  Output: { ok: bool }

GET pyramid_wire_auto_update_status
  Output: [{ schema_type: String, enabled: bool }]

# Manual "pull latest" from an update drawer
POST pyramid_wire_pull_latest
  Input: { local_contribution_id: String, latest_wire_contribution_id: String }
  Output: { new_local_contribution_id: String, activated: bool }
```

### Validation at the IPC boundary

- `pyramid_wire_discover` with unknown `sort_by` value falls back to "score" with a warning
- `pyramid_wire_recommendations` requires an existing `slug` (not NULL) — global recommendations are not meaningful because similarity needs a pyramid profile
- `pyramid_wire_pull_latest` refuses the pull if the latest version introduces new credential requirements not satisfied by the user's `.credentials` file (see `credentials-and-secrets.md`)
- `pyramid_wire_auto_update_toggle` writes a new `wire_auto_update_settings` contribution (yes, this is also a contribution — the toggle settings are themselves supersedable)

---

## Storage

### `pyramid_wire_update_cache` table

Caches the results of periodic supersession checks so the UI can show "Update available" badges without round-tripping to Wire on every render.

```sql
CREATE TABLE IF NOT EXISTS pyramid_wire_update_cache (
    local_contribution_id TEXT PRIMARY KEY
        REFERENCES pyramid_config_contributions(contribution_id),
    latest_wire_contribution_id TEXT NOT NULL,
    chain_length_delta INTEGER NOT NULL,
    changes_summary TEXT,
    author_handles_json TEXT,
    checked_at TEXT NOT NULL DEFAULT (datetime('now')),
    acknowledged_at TEXT             -- user dismissed the badge (NULL = still showing)
);
```

Entries expire when the user pulls the latest (entry deleted) or dismisses the notification (acknowledged_at set — UI suppresses the badge until next check finds an even-newer version).

### `pyramid_wire_discovery_weights` resolution

The weights used for ranking are resolved via `pyramid_active_config_contribution` with `schema_type = "wire_discovery_weights"` and `slug = NULL` (global). The result is cached in-memory with a TTL of 5 minutes, refreshed when the config is superseded.

---

## Rationale Generation

Every non-trivial ranking decision comes with a rationale string. The rationale is generated by the backend based on which signals dominated the score:

```rust
fn explain_ranking(entry: &DiscoveryResult, signals: &SignalSet) -> Option<String> {
    let mut reasons = Vec::new();

    if signals.rating_norm > 0.9 && signals.adoption_norm > 0.5 {
        reasons.push(format!("Highly rated ({}⭐) with {} adopters", entry.rating, entry.adoption_count));
    }

    if signals.chain_length >= 5 {
        reasons.push(format!("Refined over {} versions", entry.chain_length));
    }

    if signals.freshness_days < 14 {
        reasons.push(format!("Updated {}d ago", signals.freshness_days));
    }

    if signals.challenge_norm < 0.5 {
        reasons.push("Has upheld challenges against it".to_string());
    }

    if reasons.is_empty() {
        return None;
    }
    Some(reasons.join(" • "))
}
```

The rationale is displayed below the result description so the user understands the score.

---

## Frontend

### ToolsMode → Discover tab

The Discover tab in `ToolsMode.tsx` (currently a placeholder) becomes the search + recommendations surface:

- **Top section: Recommendations** — shown when a slug is selected. Up to 5 recommended configs with rationale.
- **Main section: Search** — schema_type selector, free-text query, tag filter, sort dropdown. Results rendered with quality badges and rationale.
- **Detail drawer**: clicking a result opens the `PyramidDetailDrawer` pattern with full metadata, supersession chain, related contributions, and a "Pull" button.

### ToolsMode → My Tools tab

Extended to show update badges on contributions with available updates. Badge click opens the update drawer with the changes summary and "Pull latest" action.

### Settings → Auto-Update section

Per-schema_type toggles for auto-update. Warning banner explains the behavior and the credential safety net.

---

## Files Modified

| Area | Files |
|---|---|
| Ranking engine | New `wire_discovery.rs` — signal extraction, score computation, rationale generation |
| Supersession polling | New `wire_update_poller.rs` — periodic check against Wire |
| Cache | `db.rs` — `pyramid_wire_update_cache` table |
| Weights resolution | `config_contributions.rs` — add `wire_discovery_weights` schema_type to seed bundle |
| Auto-update | `config_contributions.rs` — `wire_auto_update_settings` contribution; credential safety gate |
| IPC commands | `main.rs` or `routes.rs` — new commands in IPC Contract section |
| Frontend | `ToolsMode.tsx` — Discover tab rewrite, update badges, update drawer |
| Frontend | `Settings.tsx` — auto-update section |
| Shared components | New `QualityBadges.tsx` — rating, adoption, rebuttal, chain length, freshness |

---

## Implementation Order

1. **Ranking signal extraction** — fetch signals from Wire's search API
2. **Composite score computation** — weighted sum with the seed weights
3. **Rationale generation** — build explanation strings
4. **Discover endpoint + UI** — ship basic ranked search with quality badges
5. **Recommendations engine** — source_type + tier_routing similarity signals
6. **Supersession polling + cache** — background worker, cache table, badge integration
7. **Update drawer + pull latest** — manual update UI
8. **Auto-update toggles + credential safety gate** — opt-in automation

Phases 1-4 give users an immediately better search experience. Phases 5-8 layer on recommendations and notifications.

---

## Open Questions

1. **Ranking evaluation dataset**: How do we know the weights are any good? Recommend: ship the seed weights, collect telemetry on which results users actually pull, iterate via notes-based refinement of the weights contribution.

2. **Multi-user collaboration on weights**: Should discovery weights be per-user or shared across a circle? Recommend: per-user by default (lives in the local `pyramid_config_contributions` table); circle-level weights are a v2 feature via circle-scoped contributions.

3. **Gaming the ranking**: Authors could inflate adoption by publishing from multiple nodes. Recommend: rely on Wire's anti-sybil mechanisms; add a per-author dampener in v2 if abuse appears.

4. **Embedding similarity for apex matching**: Deferred to v2, but when implemented, should the embedding pipeline run locally or query Wire's hosted embeddings? Recommend: local — keeps discovery private, avoids leaking pyramid profiles to Wire.

5. **Notification fatigue**: If every config has an "update available" badge, users will tune them out. Recommend: auto-dismiss the badge if the delta is only the `triggering_note` wording changing (no YAML delta). Track "meaningful delta" via a diff against the pulled version's YAML.

6. **Cross-schema recommendations**: Should recommendations be able to suggest "you have an evidence_policy but no build_strategy — here are some common pairings"? Recommend: v2 feature. V1 scopes recommendations to the current schema_type only.
