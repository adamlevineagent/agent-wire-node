// pyramid/wire_discovery.rs — Phase 14: Wire Discovery Ranking +
// Recommendations Engine.
//
// Per `docs/specs/wire-discovery-ranking.md`. Phase 14 layers a ranking
// algorithm on top of Wire's raw search endpoint, a recommendations
// engine that surfaces configs similar to the user's pyramid profile,
// rationale-generation for explainable scores, and the IPC-layer
// entrypoints used by `main.rs` to expose the discovery surface to
// ToolsMode.
//
// Architectural lens: the ranking weights are themselves a contribution
// (`wire_discovery_weights` schema_type), so the composite score
// algorithm is subject to the same supersession/refinement flow as
// every other behavior in Wire Node. A user who disagrees with the
// seed weights can refine the weights contribution via notes and the
// next discovery call uses the new values.
//
// **Missing signals are neutral**, not zero. Brand-new contributions
// without adoption, rebuttals, or internalization data don't get
// dragged to the bottom of the rankings — their weight is redistributed
// across present signals. See `normalize_signals` + `compute_score`.

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::pyramid::config_contributions::load_active_config_contribution;
use crate::pyramid::wire_publish::{PyramidPublisher, WireContributionSearchResult};

// ── Ranking signals + weights ────────────────────────────────────────────────

/// Raw (un-normalized) ranking signals pulled from a Wire search result.
///
/// Each field is `Option<...>` where "missing" means the Wire has no
/// data for that signal, NOT "the value is zero". Missing signals are
/// redistributed across present signals by `normalize_signals` so new
/// contributions without adoption / rebuttals / internalization don't
/// rank at the bottom.
#[derive(Debug, Clone, Default)]
pub struct RankingSignals {
    /// 1-5 stars. `None` when the contribution has no ratings yet.
    pub rating: Option<f32>,
    /// Count of distinct nodes that pulled this contribution. `None`
    /// means Wire didn't report a value (brand-new or obscure). `Some(0)`
    /// means Wire confirmed zero adopters.
    pub adoption_count: Option<u64>,
    /// Days since last supersession or update. `None` means unknown.
    pub freshness_days: Option<u32>,
    /// Length of the supersession chain rooted at this contribution.
    /// `None` means unknown; `Some(0)` or `Some(1)` for brand-new.
    pub chain_length: Option<u32>,
    /// Wire's reputation score for the author (float in `[0, 1]`).
    /// `None` when the author is unknown / unscored.
    pub reputation: Option<f32>,
    /// Rebuttals upheld vs total filed. Both `None` means "no rebuttals
    /// tracked" and the challenge signal drops out. `Some(0), Some(0)`
    /// means "tracked, no rebuttals filed" which is a neutral signal.
    pub upheld_rebuttals: Option<u32>,
    pub filed_rebuttals: Option<u32>,
    /// Pullers who kept the contribution active (did not revert).
    /// `None` means "internalization not tracked".
    pub kept_count: Option<u64>,
    /// Total distinct pullers (denominator for the internalization
    /// rate). `None` means "internalization not tracked".
    pub total_pullers: Option<u64>,
}

impl RankingSignals {
    /// Extract ranking signals from a Wire search result entry.
    ///
    /// Heuristic: a signal counts as "missing" when the Wire's response
    /// didn't carry the field OR when the field is a zero/empty default
    /// AND we can't disambiguate "tracked, zero value" from "not
    /// tracked". For adoption / chain_length / rebuttals / pullers we
    /// treat `0` as "tracked, zero value" — the Wire serializer would
    /// emit a null for "not tracked" per the IPC contract. The only
    /// truly-ambiguous signals are rating (there's no rating of zero,
    /// so `None` unambiguously means missing) and reputation (same).
    pub fn from_search_result(r: &WireContributionSearchResult) -> Self {
        let rating = r.rating;
        // Preserve "tracked zero" vs "not tracked" for adoption. The
        // Wire IPC sends `adoption_count = 0` when it actually tracked
        // zero adopters; "not tracked" surfaces as the default `0`
        // through serde, which we can't disambiguate without a sentinel.
        // Be conservative: treat 0 adoption as `Some(0)` (tracked) so
        // older-than-180-day freshness-only signals still have
        // something to weight against.
        let adoption_count = Some(r.adoption_count);
        let freshness_days = if r.freshness_days == u32::MAX {
            None
        } else {
            Some(r.freshness_days)
        };
        let chain_length = Some(r.chain_length);
        let reputation = r.author_reputation;
        let upheld_rebuttals = Some(r.upheld_rebuttals);
        let filed_rebuttals = Some(r.filed_rebuttals);
        let (kept_count, total_pullers) = if r.total_pullers == 0 {
            // Zero total pullers means the internalization signal can't
            // be computed — mark as missing so its weight is
            // redistributed elsewhere.
            (None, None)
        } else {
            (Some(r.kept_count), Some(r.total_pullers))
        };

        Self {
            rating,
            adoption_count,
            freshness_days,
            chain_length,
            reputation,
            upheld_rebuttals,
            filed_rebuttals,
            kept_count,
            total_pullers,
        }
    }
}

/// Weights for the composite ranking score. Loaded from the
/// `wire_discovery_weights` bundled contribution (or the user's
/// refinement thereof).
///
/// All weights conventionally sum to 1.0 but the scoring algorithm
/// normalizes the present-signal weights so a non-1.0 sum or missing
/// signals don't break the ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingWeights {
    pub rating: f64,
    pub adoption: f64,
    pub freshness: f64,
    pub chain: f64,
    pub reputation: f64,
    pub challenge: f64,
    pub internalization: f64,
}

impl Default for RankingWeights {
    /// Seed weights from `wire-discovery-ranking.md` line 74-83.
    ///
    /// Tuned for "trust quality signals, penalize staleness, but don't
    /// let network effects dominate". Users can refine via notes and
    /// supersede the weights contribution.
    fn default() -> Self {
        Self {
            rating: 0.25,
            adoption: 0.20,
            freshness: 0.15,
            chain: 0.10,
            reputation: 0.10,
            challenge: 0.10,
            internalization: 0.10,
        }
    }
}

impl RankingWeights {
    /// Parse weights from a `wire_discovery_weights` YAML contribution body.
    ///
    /// Accepts either a flat map or a nested `fields:` map per the spec's
    /// example layout. Missing fields fall back to the default weights
    /// so a partial refinement (user only changes freshness) still
    /// produces a complete weight set.
    pub fn from_yaml(yaml_content: &str) -> Result<Self> {
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_content)?;
        let map = parsed.get("fields").cloned().unwrap_or(parsed);

        let default = Self::default();
        let get = |key: &str, default_val: f64| -> f64 {
            map.get(key).and_then(|v| v.as_f64()).unwrap_or(default_val)
        };

        Ok(Self {
            rating: get("w_rating", default.rating),
            adoption: get("w_adoption", default.adoption),
            freshness: get("w_freshness", default.freshness),
            chain: get("w_chain", default.chain),
            reputation: get("w_reputation", default.reputation),
            challenge: get("w_challenge", default.challenge),
            internalization: get("w_internalization", default.internalization),
        })
    }
}

/// Normalized ranking signals in `[0, 1]`. A field is `None` when the
/// raw signal was missing — the scoring pass redistributes the
/// corresponding weight across present signals.
#[derive(Debug, Clone, Default)]
pub struct NormalizedSignals {
    pub rating: Option<f64>,
    pub adoption: Option<f64>,
    pub freshness: Option<f64>,
    pub chain: Option<f64>,
    pub reputation: Option<f64>,
    pub challenge: Option<f64>,
    pub internalization: Option<f64>,
}

/// Normalize every raw signal into `[0, 1]`.
///
/// Normalization rules follow `wire-discovery-ranking.md` §Ranking
/// Signals table (line 45):
///
/// * rating:         `rating / 5.0`
/// * adoption:       `log1p(count) / log1p(max_adoption_in_set)` — log
///                   scaled, normalized against the result set max
///                   (not a global max)
/// * freshness:      `max(0, 1 - days_since_update / 180)` — linear
///                   decay over 180 days
/// * chain_length:   `min(chain_length / 10, 1.0)`
/// * reputation:     `reputation` — the Wire already returns a
///                   pre-normalized `[0, 1]` score
/// * challenge:      `1 - (upheld_rebuttals / (filed_rebuttals + 1))`
/// * internalization:`kept_count / max(1, total_pullers)`
///
/// `max_adoption_in_set` should be the maximum adoption_count observed
/// in the current search result set, NOT a global historical max.
/// Computing against the set max means a small search result with
/// modest adoption counts still gets meaningful normalized spread.
pub fn normalize_signals(signals: &RankingSignals, max_adoption_in_set: u64) -> NormalizedSignals {
    let rating = signals.rating.map(|r| (r as f64 / 5.0).clamp(0.0, 1.0));

    let adoption = signals.adoption_count.map(|count| {
        // log1p prevents divide-by-zero when max_adoption_in_set == 0.
        let numerator = (1.0 + count as f64).ln();
        let denominator = (1.0 + max_adoption_in_set as f64).ln();
        if denominator <= 0.0 {
            0.0
        } else {
            (numerator / denominator).clamp(0.0, 1.0)
        }
    });

    let freshness = signals
        .freshness_days
        .map(|days| (1.0 - (days as f64 / 180.0)).clamp(0.0, 1.0));

    let chain = signals
        .chain_length
        .map(|c| ((c as f64) / 10.0).clamp(0.0, 1.0));

    let reputation = signals.reputation.map(|r| (r as f64).clamp(0.0, 1.0));

    // Challenge combines upheld + filed. If either is missing the
    // signal drops out — we can't compute a rate.
    let challenge = match (signals.upheld_rebuttals, signals.filed_rebuttals) {
        (Some(upheld), Some(filed)) => {
            let rate = upheld as f64 / ((filed as f64) + 1.0);
            Some((1.0 - rate).clamp(0.0, 1.0))
        }
        _ => None,
    };

    let internalization = match (signals.kept_count, signals.total_pullers) {
        (Some(kept), Some(total)) if total > 0 => {
            Some(((kept as f64) / (total as f64)).clamp(0.0, 1.0))
        }
        _ => None,
    };

    NormalizedSignals {
        rating,
        adoption,
        freshness,
        chain,
        reputation,
        challenge,
        internalization,
    }
}

/// Compute a composite ranking score from normalized signals + weights.
///
/// **Missing-signal redistribution**: if a signal is `None`, its weight
/// is redistributed proportionally across the present signals. This
/// means a brand-new contribution with only a rating and a freshness
/// signal gets scored against the SUM of present-signal weights, not
/// penalized for not having adoption/rebuttals/internalization data.
///
/// Returns a score in `[0, 1]`. When every signal is missing (pure
/// empty search result), returns 0.0.
pub fn compute_score(normalized: &NormalizedSignals, weights: &RankingWeights) -> f64 {
    // Build (present_value, weight) pairs for every non-None signal.
    let pairs: Vec<(f64, f64)> = [
        (normalized.rating, weights.rating),
        (normalized.adoption, weights.adoption),
        (normalized.freshness, weights.freshness),
        (normalized.chain, weights.chain),
        (normalized.reputation, weights.reputation),
        (normalized.challenge, weights.challenge),
        (normalized.internalization, weights.internalization),
    ]
    .into_iter()
    .filter_map(|(value, weight)| value.map(|v| (v, weight)))
    .collect();

    if pairs.is_empty() {
        return 0.0;
    }

    let present_weight_sum: f64 = pairs.iter().map(|(_, w)| w).sum();
    if present_weight_sum <= 0.0 {
        return 0.0;
    }

    // Redistribute: each present signal is weighted by `weight / present_weight_sum`.
    // This is equivalent to the composite-score formula with the missing
    // weights removed and the remaining weights renormalized to sum to 1.
    pairs
        .iter()
        .map(|(value, weight)| value * (weight / present_weight_sum))
        .sum::<f64>()
        .clamp(0.0, 1.0)
}

/// Build a human-readable rationale string explaining why a result
/// scored the way it did. Returns `None` when no signal is strong
/// enough to merit an explanation — the UI then falls back to the
/// result's description.
///
/// Rationale composition rules (from the spec's `explain_ranking`
/// sketch, §Rationale Generation):
///
/// * Rating >= 0.9 normalized AND adoption >= 0.5 normalized: "Highly
///   rated (X⭐) with Y adopters"
/// * Chain length >= 5 raw: "Refined over N versions"
/// * Freshness <= 14 days raw: "Updated Nd ago"
/// * Challenge normalized < 0.5: "Has upheld challenges against it"
pub fn explain_ranking(
    entry: &WireContributionSearchResult,
    normalized: &NormalizedSignals,
) -> Option<String> {
    let mut reasons: Vec<String> = Vec::new();

    if let (Some(rating_norm), Some(adoption_norm)) = (normalized.rating, normalized.adoption) {
        if rating_norm > 0.9 && adoption_norm > 0.5 {
            let rating = entry.rating.unwrap_or(0.0);
            reasons.push(format!(
                "Highly rated ({:.1}⭐) with {} adopters",
                rating, entry.adoption_count
            ));
        }
    }

    if entry.chain_length >= 5 {
        reasons.push(format!("Refined over {} versions", entry.chain_length));
    }

    if entry.freshness_days != u32::MAX && entry.freshness_days < 14 {
        reasons.push(format!("Updated {}d ago", entry.freshness_days));
    }

    if let Some(challenge_norm) = normalized.challenge {
        if challenge_norm < 0.5 {
            reasons.push("Has upheld challenges against it".to_string());
        }
    }

    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join(" • "))
    }
}

// ── Cached-weights accessor ──────────────────────────────────────────────────

/// 5-minute TTL cache of the active `wire_discovery_weights`
/// contribution. Phase 14 spec §Storage line 318: "cached in-memory
/// with a TTL of 5 minutes, refreshed when the config is superseded".
///
/// The cache is a process-wide singleton (no state struct required).
/// On supersession, the config dispatcher calls
/// `invalidate_weights_cache()` which clears the cache so the next
/// read re-loads from the contribution store.
struct WeightsCacheEntry {
    weights: RankingWeights,
    cached_at: Instant,
}

static WEIGHTS_CACHE: LazyLock<Mutex<Option<WeightsCacheEntry>>> =
    LazyLock::new(|| Mutex::new(None));

const WEIGHTS_TTL: Duration = Duration::from_secs(5 * 60);

/// Load the active `wire_discovery_weights` contribution and return
/// the parsed weights. Uses a 5-minute in-memory TTL cache; a cache
/// miss (no entry or expired) triggers a DB read + YAML parse.
///
/// Falls back to `RankingWeights::default()` when the contribution
/// doesn't exist (first run, before the bundled manifest has been
/// walked). Logs a debug line in that case — it's the expected
/// startup behavior.
pub fn load_ranking_weights(conn: &Connection) -> RankingWeights {
    // Fast path: cache hit within TTL.
    {
        let guard = WEIGHTS_CACHE.lock().expect("weights cache poisoned");
        if let Some(entry) = guard.as_ref() {
            if entry.cached_at.elapsed() < WEIGHTS_TTL {
                return entry.weights.clone();
            }
        }
    }

    // Slow path: DB read + parse.
    let weights = match load_active_config_contribution(conn, "wire_discovery_weights", None) {
        Ok(Some(row)) => match RankingWeights::from_yaml(&row.yaml_content) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "wire_discovery_weights YAML parse failed; using defaults"
                );
                RankingWeights::default()
            }
        },
        Ok(None) => {
            tracing::debug!("wire_discovery_weights contribution not found; using seed defaults");
            RankingWeights::default()
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "wire_discovery_weights load failed; using seed defaults"
            );
            RankingWeights::default()
        }
    };

    // Refresh the cache.
    if let Ok(mut guard) = WEIGHTS_CACHE.lock() {
        *guard = Some(WeightsCacheEntry {
            weights: weights.clone(),
            cached_at: Instant::now(),
        });
    }

    weights
}

/// Clear the weights cache. Called by
/// `config_contributions::sync_config_to_operational`'s
/// `wire_discovery_weights` branch after a supersession lands so the
/// next discovery call re-reads from the contribution store.
pub fn invalidate_weights_cache() {
    if let Ok(mut guard) = WEIGHTS_CACHE.lock() {
        *guard = None;
    }
}

// ── IPC-layer types ─────────────────────────────────────────────────────────

/// One entry in the ranked discovery result set. Flattened for IPC —
/// the frontend renders `QualityBadges` from the public fields and the
/// rationale string below the description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResult {
    pub wire_contribution_id: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub author_handle: Option<String>,
    pub rating: Option<f32>,
    pub adoption_count: u64,
    pub open_rebuttals: u32,
    pub chain_length: u32,
    pub freshness_days: u32,
    /// Computed composite score in `[0, 1]`. The frontend renders this
    /// as a percentage or a progress bar.
    pub score: f64,
    /// Optional rationale string. `None` means "no signal stands out"
    /// and the UI should hide the rationale line.
    pub rationale: Option<String>,
    pub schema_type: Option<String>,
}

/// Sort mode for discovery results.
#[derive(Debug, Clone, Copy)]
pub enum DiscoverSortBy {
    Score,
    Rating,
    Adoption,
    Freshness,
    ChainLength,
}

impl DiscoverSortBy {
    pub fn from_str_lax(s: Option<&str>) -> Self {
        match s.map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("rating") => Self::Rating,
            Some("adoption") => Self::Adoption,
            Some("fresh") | Some("freshness") => Self::Freshness,
            Some("chain_length") | Some("chain") => Self::ChainLength,
            Some("score") | Some("") | None => Self::Score,
            Some(other) => {
                tracing::warn!(
                    sort_by = other,
                    "unknown sort_by value; falling back to score"
                );
                Self::Score
            }
        }
    }
}

/// Run a ranked discovery search. Calls `PyramidPublisher::search_contributions`,
/// normalizes every signal against the result set's max adoption,
/// computes composite scores with the active `wire_discovery_weights`,
/// generates rationale strings, and sorts by the requested mode.
///
/// This function performs the HTTP fetch async and then the ranking
/// work synchronously, so it does NOT hold the SQLite reader across
/// any await — the caller passes pre-loaded weights from a synchronous
/// `load_ranking_weights(&reader)` call before awaiting this function.
pub async fn discover(
    publisher: &PyramidPublisher,
    weights: RankingWeights,
    schema_type: &str,
    query: Option<&str>,
    tags: Option<&[String]>,
    limit: u32,
    sort_by: DiscoverSortBy,
) -> Result<Vec<DiscoveryResult>> {
    let raw_results = publisher
        .search_contributions(schema_type, query, tags, limit)
        .await?;

    Ok(rank_raw_results(raw_results, &weights, sort_by))
}

/// Synchronous ranking helper — applied to a pre-fetched result set.
/// Split out from `discover` so it can be unit-tested without a live
/// Wire server AND so the async `discover` path doesn't have to hold
/// the SQLite reader across the HTTP await.
pub fn rank_raw_results(
    raw_results: Vec<WireContributionSearchResult>,
    weights: &RankingWeights,
    sort_by: DiscoverSortBy,
) -> Vec<DiscoveryResult> {
    if raw_results.is_empty() {
        return Vec::new();
    }

    // Max adoption across the result set — used for log-scaled normalization.
    let max_adoption = raw_results
        .iter()
        .map(|r| r.adoption_count)
        .max()
        .unwrap_or(0);

    let mut ranked: Vec<DiscoveryResult> = raw_results
        .iter()
        .map(|r| {
            let signals = RankingSignals::from_search_result(r);
            let normalized = normalize_signals(&signals, max_adoption);
            let score = compute_score(&normalized, weights);
            let rationale = explain_ranking(r, &normalized);
            DiscoveryResult {
                wire_contribution_id: r.wire_contribution_id.clone(),
                title: r.title.clone(),
                description: r.description.clone(),
                tags: r.tags.clone(),
                author_handle: r.author_handle.clone(),
                rating: r.rating,
                adoption_count: r.adoption_count,
                open_rebuttals: r.open_rebuttals,
                chain_length: r.chain_length,
                freshness_days: r.freshness_days,
                score,
                rationale,
                schema_type: r.schema_type.clone(),
            }
        })
        .collect();

    sort_discovery_results(&mut ranked, sort_by);
    ranked
}

fn sort_discovery_results(results: &mut [DiscoveryResult], sort_by: DiscoverSortBy) {
    match sort_by {
        DiscoverSortBy::Score => {
            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        DiscoverSortBy::Rating => {
            results.sort_by(|a, b| {
                b.rating
                    .unwrap_or(0.0)
                    .partial_cmp(&a.rating.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        DiscoverSortBy::Adoption => {
            results.sort_by(|a, b| b.adoption_count.cmp(&a.adoption_count));
        }
        DiscoverSortBy::Freshness => {
            results.sort_by(|a, b| a.freshness_days.cmp(&b.freshness_days));
        }
        DiscoverSortBy::ChainLength => {
            results.sort_by(|a, b| b.chain_length.cmp(&a.chain_length));
        }
    }
}

// ── Recommendations engine ──────────────────────────────────────────────────

/// Pyramid profile used by the recommendations engine to find similar
/// pyramids on Wire. Built from local DB state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PyramidProfile {
    pub slug: String,
    pub source_type: Option<String>,
    /// Sorted unique provider_ids from `pyramid_tier_routing`.
    pub tier_routing_providers: Vec<String>,
}

/// One recommendation entry. Similar to `DiscoveryResult` but carries
/// a mandatory `rationale` (per the spec — every recommendation comes
/// with an explanation) and a similarity score instead of a generic
/// ranking score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    pub wire_contribution_id: String,
    pub title: String,
    pub description: String,
    pub rationale: String,
    /// Similarity score in `[0, 1]`.
    pub score: f64,
}

/// Build a pyramid profile from local DB state.
///
/// `source_type` resolution strategy (in order):
/// 1. `pyramid_slugs.content_type` (most reliable — set at ingest time)
/// 2. Infer from the slug's ingest configuration rows
/// 3. Fallback: `None` (profile still usable via tier_routing signal)
///
/// `tier_routing_providers` is the sorted unique list of
/// `provider_id` values from `pyramid_tier_routing`, which is the
/// global configuration — Phase 3's tier_routing is not per-slug, so
/// the list is identical across pyramids on this node. That's fine:
/// the signal measures "does this Wire contribution fit my OVERALL
/// provider setup".
pub fn build_pyramid_profile(conn: &Connection, slug: &str) -> Result<PyramidProfile> {
    // Resolve source_type from pyramid_slugs.content_type. The column
    // is NOT NULL + CHECK-constrained to the content_type vocabulary,
    // so query_row either returns a populated String or NoRows (slug
    // doesn't exist) — both of which we map to Option here.
    let source_type: Option<String> = conn
        .query_row(
            "SELECT content_type FROM pyramid_slugs WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get::<_, String>(0),
        )
        .ok();

    // Tier routing providers — read from pyramid_tier_routing.
    let mut tier_routing_providers: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT provider_id FROM pyramid_tier_routing ORDER BY provider_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(p) = row {
                out.push(p);
            }
        }
        out
    };
    tier_routing_providers.sort();
    tier_routing_providers.dedup();

    Ok(PyramidProfile {
        slug: slug.to_string(),
        source_type,
        tier_routing_providers,
    })
}

/// Compute similarity between a pyramid profile and a Wire search
/// result's adopter profile.
///
/// V1 signals (per spec §Recommendations line 131-133):
/// * `source_type_overlap`: 1.0 if the contribution's adopters include
///   a pyramid with the same source_type, 0.0 otherwise. Weight 0.6.
/// * `tier_routing_similarity`: Jaccard index of the pyramid's provider
///   set vs the contribution's adopter provider set. Weight 0.4.
///
/// Apex embedding similarity is deferred to v2.
///
/// Returns the similarity score in `[0, 1]` + a list of signal-name
/// strings that drove the match, used to build the rationale string.
pub fn compute_similarity(
    profile: &PyramidProfile,
    result: &WireContributionSearchResult,
) -> (f64, Vec<&'static str>) {
    let mut driving_signals: Vec<&'static str> = Vec::new();
    let mut score = 0.0;

    // Source type overlap (weight 0.6)
    if let Some(ref my_type) = profile.source_type {
        let matches = result
            .adopter_source_types
            .iter()
            .any(|t| t.eq_ignore_ascii_case(my_type));
        if matches {
            score += 0.6;
            driving_signals.push("source_type_overlap");
        }
    }

    // Tier routing similarity via Jaccard index (weight 0.4)
    if !profile.tier_routing_providers.is_empty() && !result.adopter_provider_ids.is_empty() {
        let my_set: std::collections::HashSet<&String> =
            profile.tier_routing_providers.iter().collect();
        let adopter_set: std::collections::HashSet<&String> =
            result.adopter_provider_ids.iter().collect();
        let intersection = my_set.intersection(&adopter_set).count();
        let union = my_set.union(&adopter_set).count();
        if union > 0 {
            let jaccard = intersection as f64 / union as f64;
            if jaccard > 0.0 {
                score += 0.4 * jaccard;
                driving_signals.push("tier_routing_similarity");
            }
        }
    }

    (score.clamp(0.0, 1.0), driving_signals)
}

/// Build a rationale string for a recommendation based on the driving
/// signals returned by `compute_similarity`.
///
/// Examples (from the spec line 124-128):
/// * "Used by N pyramids with similar tier routing"
/// * "Top-rated {schema_type} for {source_type} pyramids using local models"
/// * "Pulled by {count} users with matching tier routing"
pub fn build_recommendation_rationale(
    profile: &PyramidProfile,
    result: &WireContributionSearchResult,
    driving_signals: &[&str],
) -> String {
    let has_source_match = driving_signals.contains(&"source_type_overlap");
    let has_tier_match = driving_signals.contains(&"tier_routing_similarity");

    if has_source_match && has_tier_match {
        format!(
            "Used by {} {}-pyramids with matching tier routing",
            result.adoption_count.max(1),
            profile.source_type.as_deref().unwrap_or("similar")
        )
    } else if has_source_match {
        format!(
            "Top-rated for {} pyramids",
            profile.source_type.as_deref().unwrap_or("similar")
        )
    } else if has_tier_match {
        format!(
            "Pulled by {} users with matching tier routing",
            result.adoption_count.max(1)
        )
    } else {
        format!(
            "Popular {}",
            result.schema_type.as_deref().unwrap_or("contribution")
        )
    }
}

/// Compute recommendations for a pyramid profile against a Wire
/// schema_type. Fetches a broad candidate set from Wire, scores each
/// by similarity to the profile, and returns the top-N with rationale
/// strings.
///
/// V1 pipeline (per spec §Recommendations.pipeline):
/// 1. Load the pyramid profile from local DB (done by caller).
/// 2. Fetch a broad candidate set (`limit * 10`, capped at 100) via
///    `search_contributions`.
/// 3. Compute similarity score + rationale for each candidate.
/// 4. Filter out zero-similarity candidates (no signal matched).
/// 5. Return top-N by similarity score.
pub async fn compute_recommendations(
    publisher: &PyramidPublisher,
    profile: &PyramidProfile,
    schema_type: &str,
    limit: u32,
) -> Result<Vec<Recommendation>> {
    let candidate_limit = (limit as u64 * 10).min(100) as u32;
    let candidates = publisher
        .search_contributions(schema_type, None, None, candidate_limit)
        .await?;

    let mut scored: Vec<Recommendation> = candidates
        .iter()
        .filter_map(|r| {
            let (score, signals) = compute_similarity(profile, r);
            if score <= 0.0 {
                return None;
            }
            let rationale = build_recommendation_rationale(profile, r, &signals);
            Some(Recommendation {
                wire_contribution_id: r.wire_contribution_id.clone(),
                title: r.title.clone(),
                description: r.description.clone(),
                rationale,
                score,
            })
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(limit as usize);
    Ok(scored)
}

// ── Auto-update settings accessor ───────────────────────────────────────────

/// Parsed `wire_auto_update_settings` contribution body.
#[derive(Debug, Clone, Default)]
pub struct AutoUpdateSettings {
    /// Map schema_type → enabled. Absent keys default to `false`.
    pub enabled_by_schema: std::collections::BTreeMap<String, bool>,
}

impl AutoUpdateSettings {
    /// Parse a `wire_auto_update_settings` YAML body. Accepts either a
    /// flat map (`schema_type: bool`) or a nested `auto_update:` map.
    pub fn from_yaml(yaml_content: &str) -> Result<Self> {
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_content)?;
        let map = parsed.get("auto_update").cloned().unwrap_or(parsed);

        let mut enabled_by_schema = std::collections::BTreeMap::new();
        if let Some(mapping) = map.as_mapping() {
            for (k, v) in mapping {
                if let (Some(key), Some(val)) = (k.as_str(), v.as_bool()) {
                    // Skip schema_type meta fields (e.g. "schema_type:
                    // wire_auto_update_settings")
                    if key == "schema_type" {
                        continue;
                    }
                    enabled_by_schema.insert(key.to_string(), val);
                }
            }
        }
        Ok(Self { enabled_by_schema })
    }

    /// Check whether auto-update is enabled for a given schema type.
    /// Defaults to `false` (disabled).
    pub fn is_enabled(&self, schema_type: &str) -> bool {
        *self.enabled_by_schema.get(schema_type).unwrap_or(&false)
    }

    /// Serialize back to a YAML body for storing as a contribution.
    pub fn to_yaml(&self) -> String {
        let mut out = String::new();
        out.push_str("schema_type: wire_auto_update_settings\n");
        out.push_str("auto_update:\n");
        for (k, v) in &self.enabled_by_schema {
            out.push_str(&format!("  {k}: {v}\n"));
        }
        out
    }
}

/// Load the active `wire_auto_update_settings` contribution.
/// Returns default (all-false) when the contribution doesn't exist yet.
pub fn load_auto_update_settings(conn: &Connection) -> AutoUpdateSettings {
    match load_active_config_contribution(conn, "wire_auto_update_settings", None) {
        Ok(Some(row)) => AutoUpdateSettings::from_yaml(&row.yaml_content).unwrap_or_default(),
        _ => AutoUpdateSettings::default(),
    }
}

/// Load the `wire_update_polling` interval from its bundled
/// contribution. Returns the default interval (6 hours) when the
/// contribution doesn't exist.
pub fn load_update_polling_interval(conn: &Connection) -> Duration {
    match load_active_config_contribution(conn, "wire_update_polling", None) {
        Ok(Some(row)) => match serde_yaml::from_str::<serde_yaml::Value>(&row.yaml_content) {
            Ok(parsed) => {
                let secs = parsed
                    .get("interval_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(6 * 3600);
                Duration::from_secs(secs)
            }
            Err(_) => Duration::from_secs(6 * 3600),
        },
        _ => Duration::from_secs(6 * 3600),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod phase14_tests {
    use super::*;

    fn make_result(
        id: &str,
        rating: Option<f32>,
        adoption: u64,
        freshness: u32,
        chain: u32,
        upheld: u32,
        filed: u32,
        kept: u64,
        total: u64,
    ) -> WireContributionSearchResult {
        WireContributionSearchResult {
            wire_contribution_id: id.to_string(),
            title: format!("t-{id}"),
            description: format!("d-{id}"),
            tags: vec![],
            author_handle: Some("alice".to_string()),
            rating,
            adoption_count: adoption,
            freshness_days: freshness,
            chain_length: chain,
            upheld_rebuttals: upheld,
            filed_rebuttals: filed,
            open_rebuttals: 0,
            kept_count: kept,
            total_pullers: total,
            author_reputation: Some(0.5),
            schema_type: Some("evidence_policy".to_string()),
            adopter_provider_ids: vec!["openrouter".to_string()],
            adopter_source_types: vec!["code".to_string()],
        }
    }

    #[test]
    fn test_normalize_signals_handles_missing_signals() {
        // Brand-new contribution: no rating, no reputation, no
        // rebuttals tracked, no internalization data.
        let mut signals = RankingSignals::default();
        signals.adoption_count = Some(0);
        signals.freshness_days = Some(30);
        signals.chain_length = Some(1);
        // Ratings, reputation, rebuttals, pullers all None.

        let norm = normalize_signals(&signals, 100);
        assert!(norm.rating.is_none(), "rating missing → None");
        assert!(norm.reputation.is_none(), "reputation missing → None");
        assert!(norm.challenge.is_none(), "challenge missing → None");
        assert!(
            norm.internalization.is_none(),
            "internalization missing → None"
        );
        assert!(norm.adoption.is_some());
        assert!(norm.freshness.is_some());
        assert!(norm.chain.is_some());

        // Freshness decays linearly: 30 days → ~0.833
        let fresh = norm.freshness.unwrap();
        assert!(fresh > 0.8 && fresh < 0.9, "fresh={}", fresh);
    }

    #[test]
    fn test_compute_score_with_redistributed_weights() {
        // A signal-rich contribution should score roughly the same as
        // a signal-sparse contribution with the same normalized values
        // for the signals it has — because missing signals redistribute
        // their weight to present signals.
        let weights = RankingWeights::default();

        // Full signal set, every normalized value is 0.5.
        let full = NormalizedSignals {
            rating: Some(0.5),
            adoption: Some(0.5),
            freshness: Some(0.5),
            chain: Some(0.5),
            reputation: Some(0.5),
            challenge: Some(0.5),
            internalization: Some(0.5),
        };
        let full_score = compute_score(&full, &weights);
        assert!(
            (full_score - 0.5).abs() < 0.0001,
            "full score should be 0.5, got {}",
            full_score
        );

        // Sparse signal set (only rating + freshness), both 0.5.
        let sparse = NormalizedSignals {
            rating: Some(0.5),
            freshness: Some(0.5),
            ..NormalizedSignals::default()
        };
        let sparse_score = compute_score(&sparse, &weights);
        // The spec's missing-signal redistribution: a brand-new
        // contribution with only rating + freshness gets normalized
        // against the sum of (rating_weight + freshness_weight), so
        // its score should still be 0.5, not dragged down to
        // `(rating_weight + freshness_weight) * 0.5 = ~0.2`.
        assert!(
            (sparse_score - 0.5).abs() < 0.0001,
            "sparse score should redistribute to 0.5, got {}",
            sparse_score
        );
    }

    #[test]
    fn test_compute_score_all_missing_is_zero() {
        let empty = NormalizedSignals::default();
        let weights = RankingWeights::default();
        assert_eq!(compute_score(&empty, &weights), 0.0);
    }

    #[test]
    fn test_explain_ranking_builds_rationale_from_signals() {
        // High rating + adoption → "Highly rated ... with N adopters"
        let entry = make_result("a", Some(4.8), 200, 3, 7, 1, 10, 180, 200);
        let signals = RankingSignals::from_search_result(&entry);
        let normalized = normalize_signals(&signals, 200);
        let rationale = explain_ranking(&entry, &normalized).unwrap();
        assert!(rationale.contains("Highly rated") || rationale.contains("Refined"));
        assert!(
            rationale.contains("200")
                || rationale.contains("7 versions")
                || rationale.contains("3d")
        );
    }

    #[test]
    fn test_explain_ranking_returns_none_for_bland_result() {
        // Mid rating, small adoption, old, no chain depth.
        let entry = make_result("a", Some(3.0), 5, 120, 2, 0, 0, 3, 5);
        let signals = RankingSignals::from_search_result(&entry);
        let normalized = normalize_signals(&signals, 10);
        let rationale = explain_ranking(&entry, &normalized);
        assert!(rationale.is_none(), "expected None, got {:?}", rationale);
    }

    #[test]
    fn test_sort_discovery_results_by_score() {
        let mut results = vec![
            DiscoveryResult {
                wire_contribution_id: "low".into(),
                title: "".into(),
                description: "".into(),
                tags: vec![],
                author_handle: None,
                rating: None,
                adoption_count: 0,
                open_rebuttals: 0,
                chain_length: 0,
                freshness_days: 0,
                score: 0.3,
                rationale: None,
                schema_type: None,
            },
            DiscoveryResult {
                wire_contribution_id: "high".into(),
                title: "".into(),
                description: "".into(),
                tags: vec![],
                author_handle: None,
                rating: None,
                adoption_count: 0,
                open_rebuttals: 0,
                chain_length: 0,
                freshness_days: 0,
                score: 0.9,
                rationale: None,
                schema_type: None,
            },
        ];
        sort_discovery_results(&mut results, DiscoverSortBy::Score);
        assert_eq!(results[0].wire_contribution_id, "high");
    }

    #[test]
    fn test_recommendations_match_source_type_overlap() {
        let profile = PyramidProfile {
            slug: "my-code-pyramid".into(),
            source_type: Some("code".into()),
            tier_routing_providers: vec!["openrouter".into()],
        };
        let result = make_result("r1", Some(4.5), 50, 5, 3, 0, 0, 45, 50);
        let (score, signals) = compute_similarity(&profile, &result);
        assert!(score > 0.0);
        assert!(signals.contains(&"source_type_overlap"));
    }

    #[test]
    fn test_recommendations_match_tier_routing_similarity() {
        let profile = PyramidProfile {
            slug: "my-pyramid".into(),
            source_type: Some("document".into()), // no source match
            tier_routing_providers: vec!["openrouter".into()],
        };
        let result = make_result("r1", Some(4.5), 50, 5, 3, 0, 0, 45, 50);
        // adopter_source_types is ["code"] (no match), adopter_provider_ids is ["openrouter"] (match)
        let (score, signals) = compute_similarity(&profile, &result);
        assert!(score > 0.0);
        assert!(signals.contains(&"tier_routing_similarity"));
        assert!(!signals.contains(&"source_type_overlap"));
    }

    #[test]
    fn test_recommendations_zero_similarity_filtered() {
        let profile = PyramidProfile {
            slug: "my-pyramid".into(),
            source_type: Some("document".into()),
            tier_routing_providers: vec!["ollama".into()],
        };
        let result = make_result("r1", Some(4.5), 50, 5, 3, 0, 0, 45, 50);
        // No source match (document vs code), no tier match (ollama vs openrouter)
        let (score, _signals) = compute_similarity(&profile, &result);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_sort_by_str_lax_handles_unknown() {
        assert!(matches!(
            DiscoverSortBy::from_str_lax(Some("rating")),
            DiscoverSortBy::Rating
        ));
        assert!(matches!(
            DiscoverSortBy::from_str_lax(Some("bogus")),
            DiscoverSortBy::Score
        ));
        assert!(matches!(
            DiscoverSortBy::from_str_lax(None),
            DiscoverSortBy::Score
        ));
    }

    #[test]
    fn test_ranking_weights_from_yaml_fallback() {
        // Missing fields fall back to defaults.
        let yaml = "w_rating: 0.5\n";
        let weights = RankingWeights::from_yaml(yaml).unwrap();
        assert_eq!(weights.rating, 0.5);
        assert_eq!(weights.adoption, RankingWeights::default().adoption);
    }

    #[test]
    fn test_ranking_weights_from_yaml_nested() {
        let yaml =
            "schema_type: wire_discovery_weights\nfields:\n  w_rating: 0.3\n  w_adoption: 0.3\n";
        let weights = RankingWeights::from_yaml(yaml).unwrap();
        assert_eq!(weights.rating, 0.3);
        assert_eq!(weights.adoption, 0.3);
    }

    #[test]
    fn test_auto_update_settings_defaults_to_false() {
        let yaml = "schema_type: wire_auto_update_settings\nauto_update:\n  custom_prompts: true\n  evidence_policy: false\n";
        let settings = AutoUpdateSettings::from_yaml(yaml).unwrap();
        assert!(settings.is_enabled("custom_prompts"));
        assert!(!settings.is_enabled("evidence_policy"));
        assert!(!settings.is_enabled("unmentioned_schema"));
    }

    #[test]
    fn test_auto_update_settings_roundtrip() {
        let mut s = AutoUpdateSettings::default();
        s.enabled_by_schema.insert("custom_prompts".into(), true);
        s.enabled_by_schema.insert("evidence_policy".into(), false);
        let yaml = s.to_yaml();
        let parsed = AutoUpdateSettings::from_yaml(&yaml).unwrap();
        assert!(parsed.is_enabled("custom_prompts"));
        assert!(!parsed.is_enabled("evidence_policy"));
    }
}
