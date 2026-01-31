use crate::{interner::Interner, ortho::Ortho, ortho::payload_to_usize};

/// Returns true if the candidate should be pruned (optimistic bound cannot beat best_score).
pub fn bound_completion(
    ortho: &Ortho,
    completion: usize,
    interner: &Interner,
    best_score: (usize, usize),
) -> bool {
    let (_forbidden, required) = ortho.get_requirements();

    if required.is_empty() {
        // Empty ortho: require initial span > 1 (i.e., more than a single chain).
        let completion_count = interner
            .completions_for_prefix(&vec![completion])
            .expect("missing completions bitset for single-token prefix")
            .count_ones(..);
        return completion_count <= 1;
    }
    if best_score == (0, 0) {
        return false;
    }

    let dim_count = ortho.dims().len();
    let mut totals: Vec<usize> = Vec::with_capacity(required.len());
    for prefix in required {
        let mut prefix_usize: Vec<usize> = prefix.iter().map(|p| payload_to_usize(*p)).collect();
        prefix_usize.push(completion);
        match interner.prefix_stats(&prefix_usize) {
            Some(max_desc_len) => {
                totals.push(max_desc_len);
            }
            None => {
                panic!("[bound][panic] missing prefix stats for {:?}", prefix_usize);
            }
        }
    }

    let potential_score = upper_bound_score(&totals, ortho.volume(), dim_count);

    potential_score <= best_score
}

/// Returns true if the already-placed ortho (pre-completion) should be skipped for seeding
/// because even an optimistic continuation cannot beat best_score.
/// Missing stats or no requirements => returns false (do not prune).
pub fn bound_existing_ortho(
    ortho: &Ortho,
    interner: &Interner,
    best_score: (usize, usize),
    impacted_prefixes: Option<&[Vec<usize>]>,
) -> bool {
    if best_score == (0, 0) {
        return false;
    }
    let ortho_score = ortho.score();
    if best_score <= ortho_score {
        // Do not prune if we haven't found a strictly better score yet.
        return false;
    }
    let (_forbidden, required) = ortho.get_requirements();
    if required.is_empty() {
        return false;
    }

    let mut totals: Vec<usize> = Vec::with_capacity(required.len());
    for prefix in required {
        let prefix_usize: Vec<usize> = prefix.iter().map(|p| payload_to_usize(*p)).collect();
        match interner.prefix_stats(&prefix_usize) {
            Some(max_desc_len) => {
                totals.push(max_desc_len);
            }
            None => {
                panic!(
                    "[bound][panic] missing prefix stats for impacted prefix {:?}",
                    prefix_usize
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

    let potential_score = upper_bound_score(&axis_totals, ortho.volume(), ortho.dims().len());

    potential_score <= best_score
}

/// Compute an upper-bound (volume, fullness) given per-prefix max lengths, dim count, and a volume floor.
/// Volume upper is the saturated product of (axis total - 1) across provided axes (capped at dim_count),
/// maxed with current excess volume; fullness upper = volume upper.
pub fn upper_bound_score(
    axis_totals: &[usize],
    min_volume: usize,
    dim_count: usize,
) -> (usize, usize) {
    let mut volume_upper: usize = 1;
    for t in axis_totals.iter().take(dim_count) {
        volume_upper = volume_upper.saturating_mul(t.saturating_sub(1));
    }
    volume_upper = volume_upper.max(min_volume);
    (volume_upper, volume_upper)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{interner::Interner, ortho::PayloadVal};

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
        let should_prune = bound_completion(&ortho, a_idx, &interner, (0, 0));
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
        let should_prune = bound_completion(&ortho, b_idx, &interner, (0, 0));
        assert!(!should_prune);
    }

    #[test]
    fn bound_prunes_when_best_already_higher() {
        let interner = Interner::from_text("a b");
        let a_idx = vocab_index(&interner, "a");
        let b_idx = vocab_index(&interner, "b");

        let ortho = Ortho::new().add(PayloadVal::try_from(a_idx).unwrap())[0].clone();
        let should_prune = bound_completion(&ortho, b_idx, &interner, (10, 10));
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
        let potential_y = upper_bound_score(&totals_y, ortho.volume(), ortho.dims().len());
        let potential_z = upper_bound_score(&totals_z, ortho.volume(), ortho.dims().len());
        assert!(
            potential_y < potential_z,
            "expected deeper branch to have higher potential"
        );

        // Best score high enough to prune the shallow branch but not the deeper one.
        let best_score = (potential_z.0.saturating_sub(1), usize::MAX);

        let prunes_y = bound_completion(&ortho, y_idx, &interner, best_score);
        let prunes_z = bound_completion(&ortho, z_idx, &interner, best_score);

        assert!(
            prunes_y,
            "shallow completion y should be pruned at first slot"
        );
        assert!(!prunes_z, "deeper completion z should remain");
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
        let best_score = (0, 0);

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
}
