// compute_market.rs — Minimal Phase 1 placeholder for compute market state.
//
// Real market exchange logic, settlement, credit flows, and relay
// features arrive in Phase 2+. This file exists so the module is
// declared and importable from day one.

/// Placeholder for compute market state. Phase 2+ will add offer
/// management, job tracking, settlement, and bridge logic.
pub struct ComputeMarketState {
    pub enabled: bool,
}

impl Default for ComputeMarketState {
    fn default() -> Self {
        Self { enabled: false }
    }
}
