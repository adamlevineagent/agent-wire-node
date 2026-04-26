// pyramid/rotator_allocation.rs — Phase 5: 28-slot largest-remainder allocator.
//
// Canonical references:
//   /Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/economy/wire-rotator-arm.md
//   /Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-native-documents.md
//
// The Wire's rotator arm uses an 80-slot cycle with a fixed split:
// 48 creator / 28 source / 2 Wire / 2 Graph Fund. The 28 source slots
// are distributed across a contribution's `derived_from` entries —
// each source gets at least 1 slot, no source gets more than 28, and
// the slots must sum to EXACTLY 28.
//
// Authors declare sources with float weights at creation time. The
// canonical `wire-native-documents.md` schema shows `weight: 0.3` as
// the author-declared relative weight. At publish time, floats are
// normalized and converted to integer slots via the largest-remainder
// (Hamilton) method.
//
// 28 is a canonical protocol constant from the rotator arm, NOT a
// tunable config. The minimum-1-per-source rule is likewise a
// protocol constraint (the rotator arm wouldn't be able to route
// payments to a zero-slot source). Both are documented in
// `wire-rotator-arm.md` and inherited here. **Pillar 37 does NOT
// apply** — hardcoding 28 and min=1 are protocol alignment, not
// behavioral constraints on LLM output.

use thiserror::Error;

/// Canonical source-slot total in the 80-slot rotator arm cycle.
/// Fixed by the Wire's rotator arm protocol — NOT a tunable config.
/// Changing this would break alignment with the Wire's integer
/// revenue distribution. See `wire-rotator-arm.md`.
pub const ROTATOR_SOURCE_SLOTS: u32 = 28;

/// Canonical minimum slots per source. Fixed by the Wire's rotator
/// arm protocol — a zero-slot source would never receive a payment,
/// so the allocation algorithm must always give each cited source at
/// least 1 slot.
pub const MIN_SLOTS_PER_SOURCE: u32 = 1;

/// Canonical maximum number of sources. Derived from the combination
/// of the 28-slot total and the 1-slot-per-source minimum: with 28
/// sources each getting exactly 1 slot, the table is full.
pub const MAX_SOURCES: usize = ROTATOR_SOURCE_SLOTS as usize;

/// Errors the allocator can surface.
#[derive(Debug, Error, PartialEq)]
pub enum RotatorAllocError {
    #[error("cannot allocate 28 slots over empty weights")]
    EmptyWeights,
    #[error("too many sources: {0}, maximum is 28")]
    TooManySources(usize),
    #[error("weight at index {0} is not finite or is negative")]
    InvalidWeight(usize),
    #[error("all weights are zero; at least one source must have positive weight")]
    AllZeroWeights,
}

/// Allocate the 28 source slots among N sources using the
/// largest-remainder (Hamilton) method, then enforce the
/// minimum-1-per-source rule by reclaiming slots from the largest
/// allocations until both constraints hold.
///
/// Returns a `Vec<u32>` of length `weights.len()` where the i-th
/// entry is the slot count for source i. The sum is always exactly
/// 28 (assuming the input validates).
///
/// **Rules enforced:**
///
/// - Empty input → `EmptyWeights`
/// - More than 28 sources → `TooManySources`
/// - Any NaN/infinite/negative weight → `InvalidWeight`
/// - All-zero weights → `AllZeroWeights` (cannot normalize a zero
///   sum; caller must supply at least one positive weight)
/// - Each source receives at least 1 slot
/// - Total slot count = 28 exactly
///
/// The algorithm is deterministic: ties in the fractional-remainder
/// step are broken by the ORIGINAL index order (lower index wins),
/// so the same input always produces the same output.
pub fn allocate_28_slots(weights: &[f64]) -> Result<Vec<u32>, RotatorAllocError> {
    if weights.is_empty() {
        return Err(RotatorAllocError::EmptyWeights);
    }
    if weights.len() > MAX_SOURCES {
        return Err(RotatorAllocError::TooManySources(weights.len()));
    }
    for (i, w) in weights.iter().enumerate() {
        if !w.is_finite() || *w < 0.0 {
            return Err(RotatorAllocError::InvalidWeight(i));
        }
    }

    let sum: f64 = weights.iter().sum();
    if sum == 0.0 {
        return Err(RotatorAllocError::AllZeroWeights);
    }

    let n = weights.len();
    let target = ROTATOR_SOURCE_SLOTS;

    // Step 1: compute proportional (fractional) allocations.
    let proportional: Vec<f64> = weights.iter().map(|w| (w / sum) * target as f64).collect();

    // Step 2: floor each to an integer allocation.
    let mut slots: Vec<u32> = proportional.iter().map(|p| p.floor() as u32).collect();

    // Step 3: distribute remainder to the largest fractional parts.
    // Ties broken by lower index (stable sort).
    let allocated: u32 = slots.iter().sum();
    let mut remaining = target.saturating_sub(allocated);

    let mut remainders: Vec<(usize, f64)> = proportional
        .iter()
        .enumerate()
        .map(|(i, p)| (i, p - p.floor()))
        .collect();
    remainders.sort_by(|a, b| {
        // Descending by remainder, ascending by index on ties.
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });

    for (i, _rem) in &remainders {
        if remaining == 0 {
            break;
        }
        slots[*i] += 1;
        remaining -= 1;
    }

    // Step 4: enforce minimum-1-per-source. Any source with 0 slots
    // gets bumped to 1; the bumps are taken from the LARGEST
    // allocations (tie-broken by lower index for determinism) to keep
    // the sum at 28.
    //
    // Edge case: when n == 28 exactly, after the floor+remainder pass,
    // every source's proportional share was ≥ 1/28 of the normalized
    // weights. A degenerate input like weights = [1.0, 0.0, 0.0, ...]
    // would leave zero-weight sources at 0 slots and the positive
    // weight at 28. We fix this by reclaiming 1 slot from the
    // largest-allocated source per zero-slot source until all sources
    // are ≥ 1.
    loop {
        let zero_count: usize = slots.iter().filter(|&&s| s == 0).count();
        if zero_count == 0 {
            break;
        }

        // Find the largest allocation (tie-broken by lower index) and
        // decrement it, promoting one zero source.
        let largest_idx = slots
            .iter()
            .enumerate()
            .fold((usize::MAX, 0u32), |acc, (i, &s)| {
                if s > acc.1 || (acc.0 == usize::MAX) {
                    (i, s)
                } else {
                    acc
                }
            })
            .0;

        // Find the first zero-slot source (lowest index).
        let zero_idx = slots.iter().position(|&s| s == 0).unwrap();

        // If the largest is already at 1, we cannot legally reclaim
        // from it — but with n ≤ 28 and target 28, this is provably
        // impossible: sum(slots) = 28 and zero_count ≥ 1 means there's
        // at least one source with ≥ 2 slots.
        if slots[largest_idx] <= MIN_SLOTS_PER_SOURCE {
            // Defensive: if we somehow land here, redistribute slot
            // counts to a straight 1-per-source (safe because n ≤ 28).
            slots = vec![MIN_SLOTS_PER_SOURCE; n];
            // Distribute the remaining slots (28 - n) by weight
            // descending.
            let leftover = target - (n as u32 * MIN_SLOTS_PER_SOURCE);
            let mut indexed: Vec<(usize, f64)> =
                weights.iter().enumerate().map(|(i, w)| (i, *w)).collect();
            indexed.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.0.cmp(&b.0))
            });
            let mut remaining_leftover = leftover;
            let mut cursor = 0usize;
            while remaining_leftover > 0 {
                slots[indexed[cursor % n].0] += 1;
                remaining_leftover -= 1;
                cursor += 1;
            }
            break;
        }

        slots[largest_idx] -= 1;
        slots[zero_idx] += 1;
    }

    // Sanity check: we must ALWAYS have sum == 28.
    let final_sum: u32 = slots.iter().sum();
    debug_assert_eq!(
        final_sum, ROTATOR_SOURCE_SLOTS,
        "rotator allocation invariant violated: sum {final_sum} != {ROTATOR_SOURCE_SLOTS}"
    );

    Ok(slots)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every allocation must sum to exactly 28.
    fn assert_total_28(slots: &[u32]) {
        let sum: u32 = slots.iter().sum();
        assert_eq!(sum, 28, "allocation {slots:?} sums to {sum}, expected 28");
    }

    /// Every allocation must give each source at least 1 slot.
    fn assert_min_1(slots: &[u32]) {
        for (i, &s) in slots.iter().enumerate() {
            assert!(s >= 1, "slot[{i}] = {s}, must be >= 1 (got {slots:?})");
        }
    }

    #[test]
    fn single_source_gets_all_28() {
        let slots = allocate_28_slots(&[1.0]).unwrap();
        assert_eq!(slots, vec![28]);
    }

    #[test]
    fn single_source_arbitrary_weight() {
        let slots = allocate_28_slots(&[0.00001]).unwrap();
        assert_eq!(slots, vec![28]);
    }

    #[test]
    fn two_sources_equal_weights() {
        let slots = allocate_28_slots(&[1.0, 1.0]).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        // 28/2 = 14; both get 14.
        assert_eq!(slots, vec![14, 14]);
    }

    #[test]
    fn two_sources_three_to_one() {
        // 0.75 / 0.25 → 21 / 7
        let slots = allocate_28_slots(&[0.75, 0.25]).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        assert_eq!(slots, vec![21, 7]);
    }

    #[test]
    fn three_sources_even_thirds() {
        // 1/3 each → proportional = 9.333. Floor gives 9/9/9 = 27;
        // remainder assignment gives 1 to the first (lowest-index tie).
        let slots = allocate_28_slots(&[1.0, 1.0, 1.0]).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        // Exact largest-remainder: 9, 9, 9 + 1 extra → [10, 9, 9]
        assert_eq!(slots, vec![10, 9, 9]);
    }

    #[test]
    fn four_sources_exact_integer_split() {
        // 0.25 each → 7 each, sum 28, no remainders.
        let slots = allocate_28_slots(&[0.25, 0.25, 0.25, 0.25]).unwrap();
        assert_eq!(slots, vec![7, 7, 7, 7]);
    }

    #[test]
    fn weights_already_sum_to_28() {
        // [12, 8, 5, 3] sums to 28 — largest-remainder must return it
        // unchanged when normalized to 28.
        let slots = allocate_28_slots(&[12.0, 8.0, 5.0, 3.0]).unwrap();
        assert_eq!(slots, vec![12, 8, 5, 3]);
    }

    #[test]
    fn five_sources_with_fractional_remainders() {
        // Weights that don't divide evenly.
        // 0.2, 0.3, 0.15, 0.25, 0.1 → proportional:
        // 5.6, 8.4, 4.2, 7.0, 2.8
        // Floors: 5, 8, 4, 7, 2 → sum 26. Need 2 more.
        // Fractional parts: 0.6, 0.4, 0.2, 0.0, 0.8
        // Top 2: idx 4 (0.8), idx 0 (0.6) → +1 each.
        // Result: 6, 8, 4, 7, 3
        let slots = allocate_28_slots(&[0.2, 0.3, 0.15, 0.25, 0.1]).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        assert_eq!(slots, vec![6, 8, 4, 7, 3]);
    }

    #[test]
    fn max_28_sources_all_equal() {
        // 28 sources each with weight 1.0 — each gets exactly 1.
        let weights: Vec<f64> = vec![1.0; 28];
        let slots = allocate_28_slots(&weights).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        assert_eq!(slots, vec![1; 28]);
    }

    #[test]
    fn reject_29_sources() {
        let weights: Vec<f64> = vec![1.0; 29];
        let err = allocate_28_slots(&weights).unwrap_err();
        assert_eq!(err, RotatorAllocError::TooManySources(29));
    }

    #[test]
    fn reject_empty_weights() {
        let err = allocate_28_slots(&[]).unwrap_err();
        assert_eq!(err, RotatorAllocError::EmptyWeights);
    }

    #[test]
    fn reject_all_zero_weights() {
        let err = allocate_28_slots(&[0.0, 0.0, 0.0]).unwrap_err();
        assert_eq!(err, RotatorAllocError::AllZeroWeights);
    }

    #[test]
    fn reject_nan_weight() {
        let err = allocate_28_slots(&[1.0, f64::NAN, 1.0]).unwrap_err();
        assert_eq!(err, RotatorAllocError::InvalidWeight(1));
    }

    #[test]
    fn reject_infinite_weight() {
        let err = allocate_28_slots(&[1.0, f64::INFINITY, 1.0]).unwrap_err();
        assert_eq!(err, RotatorAllocError::InvalidWeight(1));
    }

    #[test]
    fn reject_negative_weight() {
        let err = allocate_28_slots(&[1.0, -0.5, 1.0]).unwrap_err();
        assert_eq!(err, RotatorAllocError::InvalidWeight(1));
    }

    #[test]
    fn degenerate_weight_zero_with_positive_peers() {
        // Two sources with [1.0, 0.001, 0.001]. Proportional:
        // 27.945, 0.028, 0.028. Floors: 27, 0, 0. Sum 27. Remainder 1.
        // Fractional: 0.945, 0.028, 0.028. Top 1 → idx 0.
        // Result after remainder: 28, 0, 0. Then minimum-1 reclaim:
        // 26, 1, 1.
        let slots = allocate_28_slots(&[1.0, 0.001, 0.001]).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        assert_eq!(slots, vec![26, 1, 1]);
    }

    #[test]
    fn all_mass_on_one_source_with_many_peers() {
        // One heavy source + 5 trivial — first gets most, others each
        // get at least 1 after the reclaim pass.
        let weights = vec![100.0, 0.1, 0.1, 0.1, 0.1, 0.1];
        let slots = allocate_28_slots(&weights).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        // The first source should have the majority.
        assert!(slots[0] >= 20);
    }

    #[test]
    fn deterministic_tie_breaking_on_remainder() {
        // Three sources with identical weights — tie-breaking should
        // prefer the lower index.
        let slots = allocate_28_slots(&[0.1, 0.1, 0.1]).unwrap();
        assert_eq!(slots, vec![10, 9, 9]);
    }

    #[test]
    fn two_sources_with_one_heavy_dominant() {
        // 0.99 / 0.01 → 27.72 / 0.28 → floors 27/0, remainder goes to
        // idx 0 (highest remainder) → 28/0, then reclaim → 27/1.
        let slots = allocate_28_slots(&[0.99, 0.01]).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        assert_eq!(slots, vec![27, 1]);
    }

    #[test]
    fn seven_sources_varying_weights() {
        // Realistic mix — verify invariants without asserting exact
        // distribution.
        let weights = vec![0.3, 0.25, 0.15, 0.1, 0.1, 0.05, 0.05];
        let slots = allocate_28_slots(&weights).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        assert_eq!(slots.len(), 7);
        // Heaviest source must have the most slots (or tied for most).
        let max_slots = *slots.iter().max().unwrap();
        assert_eq!(slots[0], max_slots);
    }

    #[test]
    fn twenty_eight_unequal_sources() {
        // 28 sources with geometric decay. Each should get exactly 1
        // after the minimum-1 reclaim pass.
        let weights: Vec<f64> = (0..28).map(|i| 1.0 / (1.0 + i as f64)).collect();
        let slots = allocate_28_slots(&weights).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        // With 28 sources and 28 slots, every source gets exactly 1
        // after the reclaim pass.
        assert_eq!(slots, vec![1; 28]);
    }

    #[test]
    fn large_spread_with_seventeen_sources() {
        // Verify minimum-1 enforcement on a realistic mix.
        let weights = vec![
            10.0, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01,
            0.01, 0.01, 0.01,
        ];
        let slots = allocate_28_slots(&weights).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        // First should have the largest share.
        let max_slots = *slots.iter().max().unwrap();
        assert_eq!(slots[0], max_slots);
    }

    #[test]
    fn canonical_example_three_sources() {
        // Exactly mirrors the canonical example from
        // wire-native-documents.md → derived_from (lines 49-52):
        //   ref: nightingale/77/3 weight 0.3
        //   doc: wire-actions.md  weight 0.3
        //   doc: synthesis-primitives.md weight 0.2
        //
        // Normalized sum = 0.8; proportional = 10.5/10.5/7.0.
        // Floors 10/10/7 → sum 27. Remainder 1 distributed to idx 0
        // (first highest-fractional tie). Result: 11/10/7.
        let slots = allocate_28_slots(&[0.3, 0.3, 0.2]).unwrap();
        assert_total_28(&slots);
        assert_min_1(&slots);
        assert_eq!(slots, vec![11, 10, 7]);
    }
}
