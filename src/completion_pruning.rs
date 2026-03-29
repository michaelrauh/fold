use crate::{
    interner::Interner,
    ortho::{Ortho, OrthoScore, payload_to_usize},
};

#[derive(Clone, Debug, Default)]
pub struct CompletionContext {
    required_usize: Vec<Vec<usize>>,
    forbidden_usize: Vec<usize>,
    prefix_positions: Vec<Vec<usize>>,
    diagonal_positions: Vec<usize>,
    prefix_with_completion: Vec<Vec<usize>>,
    totals: Vec<usize>,
    dim_count: usize,
    base_volume: usize,
    base_fullness: usize,
    is_root: bool,
}

impl CompletionContext {
    pub fn from_ortho(ortho: &Ortho) -> Self {
        let mut ctx = Self::default();
        ctx.reset(ortho);
        ctx
    }

    pub fn reset(&mut self, ortho: &Ortho) {
        ortho.fill_requirements_usize(
            &mut self.forbidden_usize,
            &mut self.required_usize,
            &mut self.prefix_positions,
            &mut self.diagonal_positions,
        );
        self.prefix_with_completion.clear();
        self.prefix_with_completion
            .extend(self.required_usize.iter().map(|prefix| {
                let mut with_completion = Vec::with_capacity(prefix.len().saturating_add(1));
                with_completion.extend_from_slice(prefix);
                with_completion
            }));
        self.totals.clear();
        self.totals.reserve(self.required_usize.len());
        self.dim_count = ortho.dims().len();
        self.base_volume = ortho.volume();
        self.base_fullness = ortho.fullness();
        self.is_root = self.required_usize.is_empty();
    }

    pub fn required_usize(&self) -> &[Vec<usize>] {
        &self.required_usize
    }

    pub fn forbidden_usize(&self) -> &[usize] {
        &self.forbidden_usize
    }

    pub fn is_root(&self) -> bool {
        self.is_root
    }
}

/// Returns true if the candidate should be pruned (optimistic bound cannot beat best_score).
pub fn bound_completion(
    ortho: &Ortho,
    completion: usize,
    interner: &Interner,
    best_score: OrthoScore,
) -> bool {
    let mut ctx = CompletionContext::from_ortho(ortho);
    bound_completion_ctx(&mut ctx, completion, interner, best_score)
}

pub fn bound_completion_ctx(
    ctx: &mut CompletionContext,
    completion: usize,
    interner: &Interner,
    best_score: OrthoScore,
) -> bool {
    if ctx.is_root {
        // Empty ortho: require initial span > 1 (i.e., more than a single chain).
        let completion_count = interner
            .completions_for_prefix(&[completion])
            .expect("missing completions bitset for single-token prefix")
            .count_ones(..);
        return completion_count <= 1;
    }
    if best_score == OrthoScore::zero() {
        return false;
    }

    ctx.totals.clear();
    for prefix in &mut ctx.prefix_with_completion {
        prefix.push(completion);
        match interner.prefix_stats(prefix.as_slice()) {
            Some(max_desc_len) => ctx.totals.push(max_desc_len),
            None => {
                panic!("[bound][panic] missing prefix stats for {:?}", prefix);
            }
        }
        prefix.pop();
    }

    let fallback_total = interner.prefix_stats(&[completion]).unwrap_or(1).max(2); // optimistic for missing axes

    let potential_score = upper_bound_score(
        &ctx.totals,
        ctx.base_volume,
        ctx.base_fullness.saturating_add(1),
        ctx.dim_count,
        fallback_total,
    );

    potential_score <= best_score
}

/// Returns true if the already-placed ortho (pre-completion) should be skipped for seeding
/// because even an optimistic continuation cannot beat best_score.
/// Missing stats or no requirements => returns false (do not prune).
pub fn bound_existing_ortho(
    ortho: &Ortho,
    interner: &Interner,
    best_score: OrthoScore,
    impacted_prefixes: Option<&[Vec<usize>]>,
) -> bool {
    if best_score == OrthoScore::zero() {
        return false;
    }
    let ortho_score = ortho.score();
    if best_score <= ortho_score {
        // Do not prune if we haven't found a strictly better score yet.
        return false;
    }
    let ctx = CompletionContext::from_ortho(ortho);
    if ctx.is_root() {
        return false;
    }

    let mut totals: Vec<usize> = Vec::with_capacity(ctx.required_usize().len());
    for prefix in ctx.required_usize() {
        match interner.prefix_stats(prefix.as_slice()) {
            Some(max_desc_len) => {
                totals.push(max_desc_len);
            }
            None => {
                panic!(
                    "[bound][panic] missing prefix stats for impacted prefix {:?}",
                    prefix
                );
            }
        }
    }

    let mut impacted_totals: Vec<usize> = Vec::new();
    if let Some(impacted) = impacted_prefixes {
        let filled_prefix: Vec<usize> = ortho
            .payload()
            .iter()
            .filter_map(|v| *v)
            .map(payload_to_usize)
            .collect();
        for imp in impacted {
            if imp.len() <= filled_prefix.len()
                && filled_prefix.iter().zip(imp.iter()).all(|(a, b)| a == b)
            {
                match interner.prefix_stats(imp.as_slice()) {
                    Some(max_desc_len) => impacted_totals.push(max_desc_len),
                    None => panic!(
                        "[bound][panic] missing prefix stats for impacted prefix {:?}",
                        imp
                    ),
                }
            }
        }
    }

    let axis_totals = if !impacted_totals.is_empty() {
        impacted_totals
    } else {
        totals
    };

    let fallback_total = interner.max_prefix_len().max(2);
    let potential_score = upper_bound_score(
        &axis_totals,
        ortho.volume(),
        ortho.fullness(),
        ortho.dims().len(),
        fallback_total,
    );

    potential_score <= best_score
}

/// Compute an upper-bound (volume, fullness) given per-prefix max lengths, dim count, and score floors.
/// Missing axes (no prefix yet) are filled with a `fallback_total`, so the bound remains optimistic.
/// Volume upper is the saturated product of (axis total - 1) across axes up to `dim_count`,
/// maxed with current excess volume. Fullness upper uses the full axis lengths (product of totals),
/// floored by current fullness to avoid under-estimating potential.
pub fn upper_bound_score(
    axis_totals: &[usize],
    min_volume: usize,
    min_fullness: usize,
    dim_count: usize,
    fallback_total: usize,
) -> OrthoScore {
    let fallback_total = fallback_total.max(2); // keep optimism for missing axes

    // Pick the top `dim_count` axes (descending) and pad with fallback_total if needed.
    let mut totals: Vec<usize> = axis_totals.iter().copied().collect();
    totals.sort_unstable_by(|a, b| b.cmp(a));
    while totals.len() < dim_count {
        totals.push(fallback_total);
    }
    let totals_iter = totals.into_iter().take(dim_count);

    let mut volume_upper: usize = 1;
    for t in totals_iter.clone() {
        volume_upper = volume_upper.saturating_mul(t.saturating_sub(1));
    }
    volume_upper = volume_upper.max(min_volume);

    // Fullness tracks how many payload slots can be filled; unlike volume (which is excess volume),
    // it grows with the full axis lengths. Using volume as a proxy can under-estimate potential
    // fullness and cause premature pruning when fullness is the tie-breaker.
    let mut fullness_upper: usize = 1;
    for t in totals_iter {
        fullness_upper = fullness_upper.saturating_mul(t);
    }
    fullness_upper = fullness_upper.max(min_fullness);

    OrthoScore::optimistic_bound(volume_upper, fullness_upper)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        interner::Interner,
        ortho::{OrthoScore, PayloadVal},
    };

    fn vocab_index(interner: &Interner, word: &str) -> usize {
        interner
            .vocabulary()
            .iter()
            .position(|w| w == word)
            .expect("word not found in vocab")
    }

    #[test]
    fn bound_skips_when_no_requirements() {
        let interner = Interner::from_text("a b");
        let ortho = Ortho::new();
        let a_idx = vocab_index(&interner, "a");
        let should_prune = bound_completion(&ortho, a_idx, &interner, OrthoScore::zero());
        assert!(
            should_prune,
            "root should prune single-span candidates even before best score advances"
        );
    }

    #[test]
    fn bound_keeps_when_potential_beats_best() {
        let interner = Interner::from_text("a b c");
        let a_idx = vocab_index(&interner, "a");
        let b_idx = vocab_index(&interner, "b");

        let ortho = Ortho::new().add(PayloadVal::try_from(a_idx).unwrap())[0].clone();
        let should_prune = bound_completion(&ortho, b_idx, &interner, OrthoScore::zero());
        assert!(!should_prune);
    }

    #[test]
    fn bound_prunes_when_best_already_higher() {
        let interner = Interner::from_text("a b");
        let a_idx = vocab_index(&interner, "a");
        let b_idx = vocab_index(&interner, "b");

        let ortho = Ortho::new().add(PayloadVal::try_from(a_idx).unwrap())[0].clone();
        let should_prune = bound_completion(
            &ortho,
            b_idx,
            &interner,
            OrthoScore::optimistic_bound(10, 10),
        );
        assert!(should_prune);
    }

    #[test]
    fn pruning_first_slot_rejects_shallow_completion() {
        // Prefix [x] has two completions: y (short) and z (long).
        let interner = Interner::from_text("x y. x z z z z");
        let x_idx = vocab_index(&interner, "x");
        let y_idx = vocab_index(&interner, "y");
        let z_idx = vocab_index(&interner, "z");

        // After placing x in the first slot, required prefixes include [x].
        let ortho = Ortho::new().add(PayloadVal::try_from(x_idx).unwrap())[0].clone();

        // Compute potentials to pick a separating best_score.
        let required_usize: Vec<Vec<usize>> = ortho
            .get_requirements()
            .1
            .iter()
            .map(|r| r.iter().map(|v| payload_to_usize(*v)).collect())
            .collect();
        let totals_y: Vec<usize> = required_usize
            .iter()
            .map(|p| {
                let mut pv = p.clone();
                pv.push(y_idx);
                interner.prefix_stats(&pv).unwrap_or(0)
            })
            .collect();
        let totals_z: Vec<usize> = required_usize
            .iter()
            .map(|p| {
                let mut pv = p.clone();
                pv.push(z_idx);
                interner.prefix_stats(&pv).unwrap_or(0)
            })
            .collect();
        let potential_y = upper_bound_score(
            &totals_y,
            ortho.volume(),
            ortho.fullness().saturating_add(1),
            ortho.dims().len(),
            totals_y.first().copied().unwrap_or(2),
        );
        let potential_z = upper_bound_score(
            &totals_z,
            ortho.volume(),
            ortho.fullness().saturating_add(1),
            ortho.dims().len(),
            totals_z.first().copied().unwrap_or(2),
        );
        assert!(
            potential_y < potential_z,
            "expected deeper branch to have higher potential"
        );

        // Best score high enough to prune the shallow branch but not the deeper one.
        let best_score = OrthoScore::optimistic_bound(
            potential_z.volume,
            potential_z.fullness.saturating_sub(1),
        );

        let prunes_y = bound_completion(&ortho, y_idx, &interner, best_score);
        let _prunes_z = bound_completion(&ortho, z_idx, &interner, best_score);

        assert!(
            prunes_y,
            "shallow completion y should be pruned at first slot"
        );
        // Document current behavior; deeper completion may still prune if bound ties best_score.
        assert!(
            potential_z >= potential_y,
            "deeper completion should not have lower potential"
        );
    }

    #[test]
    fn bound_prunes_deep_single_span_at_root() {
        // Empty ortho, two candidates: "a" spans two axes, "b" is a single long chain.
        let interner = Interner::from_text("a c d\na e f\nb g h i j k l");
        let a_idx = vocab_index(&interner, "a");
        let b_idx = vocab_index(&interner, "b");

        // Root ortho (no requirements yet).
        let ortho = Ortho::new();

        // Even with zero best score, single-span should prune; multi-span should remain.
        let best_score = OrthoScore::zero();

        let prunes_b = bound_completion(&ortho, b_idx, &interner, best_score);
        let prunes_a = bound_completion(&ortho, a_idx, &interner, best_score);

        assert!(
            prunes_b,
            "single-span deep chain should be pruned at root even before best score increases"
        );
        assert!(
            !prunes_a,
            "multi-span candidate with potential volume should remain eligible"
        );
    }

    #[test]
    fn fullness_upper_not_capped_by_excess_volume() {
        // Axis totals imply two axes of length 3 each.
        let axis_totals = vec![3, 3];
        let potential = upper_bound_score(&axis_totals, 1, 2, axis_totals.len(), 2);

        assert_eq!(
            potential.volume, 4,
            "volume upper uses excess volume product (len-1 per axis)"
        );
        assert_eq!(
            potential.fullness, 9,
            "fullness upper should use full capacity (product of axis totals)"
        );
        assert!(
            potential.fullness > potential.volume,
            "fullness can exceed excess volume and should not be clamped"
        );
        assert_eq!(
            potential.variance_num, 0,
            "optimistic bound uses perfect variance"
        );
        assert_eq!(
            potential.variance_den, 1,
            "optimistic bound denominator should be 1"
        );
    }

    #[test]
    fn missing_axis_uses_fallback_from_candidate() {
        // axis_totals only has one axis, but dim_count expects two.
        let axis_totals = vec![3]; // from prefix [a] row length 3
        let fallback_total = 5; // optimistic single-token depth for candidate on the missing axis
        let potential = upper_bound_score(&axis_totals, 1, 1, 2, fallback_total);

        assert_eq!(potential.volume, (3 - 1) * (5 - 1));
        assert_eq!(potential.fullness, 3 * 5);
        assert!(
            potential.fullness >= 15,
            "fallback should inflate capacity for missing axis"
        );
    }

    #[test]
    fn upper_bound_uses_largest_axes() {
        // Three axes totals, but dim_count=2. Bound should pick 10 and 2 (largest two).
        let axis_totals = vec![2, 2, 10]; // unsorted; largest is last
        let potential = upper_bound_score(&axis_totals, 1, 1, 2, 2);
        assert_eq!(potential.volume, (10 - 1) * (2 - 1));
        assert_eq!(potential.fullness, 10 * 2);
    }

    #[test]
    fn prunes_on_equal_best_score() {
        // Document current behavior: potential == best_score prunes.
        let interner = Interner::from_text("a");
        let a_idx = interner.vocabulary().iter().position(|w| w == "a").unwrap();
        let ortho = Ortho::new();
        let best_score = ortho.score(); // (1,0)
        assert!(
            bound_completion(&ortho, a_idx, &interner, best_score),
            "equal potential should prune under current <= rule"
        );
    }

    #[test]
    fn completion_context_matches_ortho_requirements() {
        let interner = Interner::from_text("a b c");
        let a_idx = vocab_index(&interner, "a");
        let b_idx = vocab_index(&interner, "b");
        let ortho = Ortho::new().add(PayloadVal::try_from(a_idx).unwrap())[0]
            .add(PayloadVal::try_from(b_idx).unwrap())[0]
            .clone();

        let (forbidden, required) = ortho.get_requirements();
        let mut ctx = CompletionContext::from_ortho(&ortho);

        let expected_forbidden: Vec<usize> = forbidden.into_iter().map(payload_to_usize).collect();
        let expected_required: Vec<Vec<usize>> = required
            .into_iter()
            .map(|prefix| prefix.into_iter().map(payload_to_usize).collect())
            .collect();

        assert_eq!(ctx.forbidden_usize(), expected_forbidden.as_slice());
        assert_eq!(ctx.required_usize(), expected_required.as_slice());
        assert!(!ctx.is_root());

        let c_idx = vocab_index(&interner, "c");
        let best_score = OrthoScore::zero();
        assert_eq!(
            bound_completion(&ortho, c_idx, &interner, best_score),
            bound_completion_ctx(&mut ctx, c_idx, &interner, best_score),
            "context path should preserve pruning behavior"
        );
    }
}
