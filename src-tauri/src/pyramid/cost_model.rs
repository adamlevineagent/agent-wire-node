// pyramid/cost_model.rs — WS-COST-MODEL (Phase 1 of Episodic Memory Vine canonical v4)
//
// Purpose: transparency + preview-gate cost estimation for chain phases.
//
// Contract (see docs/plans/episodic-memory-vine-canonical-v4.md §15.17, §16.1):
//   - On lookup: query pyramid_llm_audit for observed per-(chain_phase, model) averages.
//     If observations exist, use them.
//   - If pyramid_llm_audit is empty for the (chain_phase, model) key: fall back to seed
//     heuristics marked `is_heuristic: true`.
//   - After each build completes, observations OVERWRITE the seed row for touched keys
//     (recompute on build-complete event, OR on demand via the admin endpoint).
//   - Cost lookup returns USD estimate for any (chain_phase, model) pair.
//
// Cost is NOT a primary concern — this module exists for transparency and the
// preview gate. Rates come from Tier1Config pricing (pyramid_config.json), which is
// also the per-model price table used by config_helper::estimate_cost.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A single (chain_phase, model) cost-model row.
///
/// `chain_phase` is the executor step_name as recorded in `pyramid_llm_audit.step_name`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostModelEntry {
    pub chain_phase: String,
    pub model: String,
    /// Average input tokens per call observed (or seeded).
    pub avg_input_tokens: f64,
    /// Average output tokens per call observed (or seeded).
    pub avg_output_tokens: f64,
    /// Typical number of calls per conversation/build for this phase.
    pub calls_per_conversation: f64,
    /// USD cost per single call at the rates recorded when this row was written.
    pub usd_per_call: f64,
    /// USD cost for a typical conversation/build at this phase.
    pub usd_per_conversation: f64,
    /// True iff this row came from the seed heuristics table (no observations yet).
    pub is_heuristic: bool,
    /// Sample size (number of audit rows averaged). 0 for heuristic seeds.
    pub sample_count: u64,
    /// Unix seconds when this row was last written.
    pub updated_at: i64,
}

/// Seed file layout. Loaded from `chains/defaults/pyramid_chain_cost_model_seed.json`.
///
/// The JSON mirrors the heuristic arithmetic from the plan:
///   ~8k input + ~1.5k output per extraction call
///   ~20 calls per conversation in fast mode (~$0.20)
///   ~50 calls per conversation in deep mode (~$0.80)
/// using the per-million-token prices from pyramid_config.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostModelSeed {
    pub version: u32,
    pub entries: Vec<CostModelSeedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostModelSeedEntry {
    pub chain_phase: String,
    pub model: String,
    pub avg_input_tokens: f64,
    pub avg_output_tokens: f64,
    pub calls_per_conversation: f64,
    /// Optional override for input $/Mtok (otherwise use caller-supplied default).
    #[serde(default)]
    pub input_price_per_million: Option<f64>,
    /// Optional override for output $/Mtok.
    #[serde(default)]
    pub output_price_per_million: Option<f64>,
}

/// Compute USD per call given token counts and per-million-token rates.
#[inline]
pub fn cost_per_call(
    in_tokens: f64,
    out_tokens: f64,
    in_price_per_m: f64,
    out_price_per_m: f64,
) -> f64 {
    (in_tokens * in_price_per_m + out_tokens * out_price_per_m) / 1_000_000.0
}

/// Load the seed JSON. Returns an empty seed on missing/invalid file (non-fatal).
pub fn load_seed(path: &Path) -> CostModelSeed {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<CostModelSeed>(&s).unwrap_or_else(|e| {
            eprintln!("[cost_model] seed parse error at {path:?}: {e}");
            CostModelSeed {
                version: 1,
                entries: Vec::new(),
            }
        }),
        Err(_) => CostModelSeed {
            version: 1,
            entries: Vec::new(),
        },
    }
}

/// Seed the table with heuristic rows for any (chain_phase, model) keys that
/// do not already have observed rows. Called on cold start.
pub fn apply_seed(
    conn: &Connection,
    seed: &CostModelSeed,
    default_in_price_per_m: f64,
    default_out_price_per_m: f64,
) -> Result<usize> {
    let now = now_secs();
    let mut inserted = 0usize;
    for e in &seed.entries {
        let in_p = e.input_price_per_million.unwrap_or(default_in_price_per_m);
        let out_p = e
            .output_price_per_million
            .unwrap_or(default_out_price_per_m);
        let per_call = cost_per_call(e.avg_input_tokens, e.avg_output_tokens, in_p, out_p);
        let per_conv = per_call * e.calls_per_conversation;

        // Only write heuristic row if this (phase, model) has no row yet.
        // Observed rows are never overwritten by seed data.
        let existing: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM pyramid_chain_cost_model WHERE chain_phase = ?1 AND model = ?2",
                params![e.chain_phase, e.model],
                |r| r.get(0),
            )
            .optional()?;
        if existing.is_some() {
            continue;
        }
        conn.execute(
            "INSERT INTO pyramid_chain_cost_model
                (chain_phase, model, avg_input_tokens, avg_output_tokens,
                 calls_per_conversation, usd_per_call, usd_per_conversation,
                 is_heuristic, sample_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 0, ?8)",
            params![
                e.chain_phase,
                e.model,
                e.avg_input_tokens,
                e.avg_output_tokens,
                e.calls_per_conversation,
                per_call,
                per_conv,
                now,
            ],
        )?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Recompute observed rows from `pyramid_llm_audit`.
///
/// For every (step_name, model) pair with at least one completed audit row, compute
/// the observed averages and overwrite the cost-model row (marking it non-heuristic).
/// Untouched (phase, model) pairs are left alone — heuristic seeds for other
/// phases/models remain intact.
///
/// `calls_per_conversation` is computed as `total_calls / distinct_builds`.
///
/// Returns the number of rows upserted.
pub fn recompute_from_audit(
    conn: &Connection,
    default_in_price_per_m: f64,
    default_out_price_per_m: f64,
) -> Result<usize> {
    let now = now_secs();
    let mut stmt = conn.prepare(
        "SELECT step_name,
                model,
                AVG(CAST(prompt_tokens AS REAL))       AS avg_in,
                AVG(CAST(completion_tokens AS REAL))   AS avg_out,
                COUNT(*)                               AS n,
                COUNT(DISTINCT build_id)               AS builds
         FROM pyramid_llm_audit
         WHERE status = 'complete'
           AND prompt_tokens > 0
         GROUP BY step_name, model",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;

    let mut upserted = 0usize;
    for row in rows {
        let (phase, model, avg_in, avg_out, n, builds) = row?;
        let builds = builds.max(1);
        let calls_per_conv = (n as f64) / (builds as f64);
        let per_call = cost_per_call(avg_in, avg_out, default_in_price_per_m, default_out_price_per_m);
        let per_conv = per_call * calls_per_conv;

        conn.execute(
            "INSERT INTO pyramid_chain_cost_model
                (chain_phase, model, avg_input_tokens, avg_output_tokens,
                 calls_per_conversation, usd_per_call, usd_per_conversation,
                 is_heuristic, sample_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9)
             ON CONFLICT(chain_phase, model) DO UPDATE SET
                avg_input_tokens       = excluded.avg_input_tokens,
                avg_output_tokens      = excluded.avg_output_tokens,
                calls_per_conversation = excluded.calls_per_conversation,
                usd_per_call           = excluded.usd_per_call,
                usd_per_conversation   = excluded.usd_per_conversation,
                is_heuristic           = 0,
                sample_count           = excluded.sample_count,
                updated_at             = excluded.updated_at",
            params![
                phase,
                model,
                avg_in,
                avg_out,
                calls_per_conv,
                per_call,
                per_conv,
                n as i64,
                now,
            ],
        )?;
        upserted += 1;
    }
    Ok(upserted)
}

/// Look up cost for a (chain_phase, model). Returns `None` if no row exists
/// (caller should have seeded before lookup for full coverage).
pub fn lookup(
    conn: &Connection,
    chain_phase: &str,
    model: &str,
) -> Result<Option<CostModelEntry>> {
    let row = conn
        .query_row(
            "SELECT chain_phase, model, avg_input_tokens, avg_output_tokens,
                    calls_per_conversation, usd_per_call, usd_per_conversation,
                    is_heuristic, sample_count, updated_at
             FROM pyramid_chain_cost_model
             WHERE chain_phase = ?1 AND model = ?2",
            params![chain_phase, model],
            |r| {
                Ok(CostModelEntry {
                    chain_phase: r.get(0)?,
                    model: r.get(1)?,
                    avg_input_tokens: r.get(2)?,
                    avg_output_tokens: r.get(3)?,
                    calls_per_conversation: r.get(4)?,
                    usd_per_call: r.get(5)?,
                    usd_per_conversation: r.get(6)?,
                    is_heuristic: r.get::<_, i64>(7)? != 0,
                    sample_count: r.get::<_, i64>(8)?.max(0) as u64,
                    updated_at: r.get(9)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Dump all rows, grouped by chain_phase.
pub fn list_all(conn: &Connection) -> Result<BTreeMap<String, Vec<CostModelEntry>>> {
    let mut stmt = conn.prepare(
        "SELECT chain_phase, model, avg_input_tokens, avg_output_tokens,
                calls_per_conversation, usd_per_call, usd_per_conversation,
                is_heuristic, sample_count, updated_at
         FROM pyramid_chain_cost_model
         ORDER BY chain_phase, model",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(CostModelEntry {
            chain_phase: r.get(0)?,
            model: r.get(1)?,
            avg_input_tokens: r.get(2)?,
            avg_output_tokens: r.get(3)?,
            calls_per_conversation: r.get(4)?,
            usd_per_call: r.get(5)?,
            usd_per_conversation: r.get(6)?,
            is_heuristic: r.get::<_, i64>(7)? != 0,
            sample_count: r.get::<_, i64>(8)?.max(0) as u64,
            updated_at: r.get(9)?,
        })
    })?;
    let mut out: BTreeMap<String, Vec<CostModelEntry>> = BTreeMap::new();
    for r in rows {
        let e = r?;
        out.entry(e.chain_phase.clone()).or_default().push(e);
    }
    Ok(out)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
