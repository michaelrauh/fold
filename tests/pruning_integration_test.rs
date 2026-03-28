use fold::completion_pruning::{bound_completion, bound_existing_ortho, upper_bound_score};
use fold::interner::Interner;
use fold::ortho::{Ortho, OrthoScore, PayloadVal};

fn vocab_index(interner: &Interner, word: &str) -> usize {
    interner
        .vocabulary()
        .iter()
        .position(|w| w == word)
        .expect("word not in vocab")
}

#[test]
fn pruning_skips_low_potential_completion_but_keeps_higher() {
    // Build interner where prefix [a] can end quickly (a b) or continue much longer.
    let interner = Interner::from_text("a b. a c d e f g h i j");
    let a_idx = vocab_index(&interner, "a");
    let b_idx = vocab_index(&interner, "b");
    let c_idx = vocab_index(&interner, "c");

    let total_ab = interner.prefix_stats(&vec![a_idx, b_idx]).unwrap_or(0);
    let total_ac = interner.prefix_stats(&vec![a_idx, c_idx]).unwrap_or(0);
    assert!(
        total_ac > total_ab,
        "expected longer total depth for [a c] than [a b]"
    );

    // Ortho with a single token so required prefixes include [a]
    let ortho = Ortho::new().add(PayloadVal::try_from(a_idx).unwrap())[0].clone();

    // Compute upper-bound potentials for each completion using axis totals.
    let potential_b = upper_bound_score(
        &[total_ab],
        ortho.volume(),
        ortho.fullness().saturating_add(1),
        ortho.dims().len(),
        total_ab,
    );
    let potential_c = upper_bound_score(
        &[total_ac],
        ortho.volume(),
        ortho.fullness().saturating_add(1),
        ortho.dims().len(),
        total_ac,
    );
    assert!(
        potential_c > potential_b,
        "longer branch should have higher potential"
    );

    // Choose a best_score that prunes the short branch ([a b]) but not the longer one ([a c ...]).
    let best_score = if potential_c.volume > potential_b.volume {
        OrthoScore::optimistic_bound(potential_c.volume.saturating_sub(1), usize::MAX)
    } else {
        OrthoScore::optimistic_bound(potential_c.volume, potential_c.fullness.saturating_sub(1))
    };
    assert!(best_score >= potential_b);
    assert!(best_score < potential_c);

    let completions = interner.intersect(&vec![vec![a_idx]], &[]);
    assert!(completions.contains(&b_idx));
    assert!(completions.contains(&c_idx));

    let pruned: Vec<usize> = completions
        .iter()
        .copied()
        .filter(|c| bound_completion(&ortho, *c, &interner, best_score))
        .collect();
    let kept: Vec<usize> = completions
        .iter()
        .copied()
        .filter(|c| !bound_completion(&ortho, *c, &interner, best_score))
        .collect();

    assert!(
        potential_c > potential_b,
        "longer branch should have higher potential"
    );
    assert!(
        pruned.len() + kept.len() == completions.len(),
        "pruned+kept should partition completions"
    );
    assert!(
        kept.len() + pruned.len() == completions.len(),
        "pruned+kept should partition completions"
    );
}

#[test]
fn impacted_seeding_prunes_hopeless_prefixes() {
    // Same corpus: [a b] cannot grow; [a c] can extend.
    let interner = Interner::from_text("a b. a c d e f g h i j");
    let a_idx = vocab_index(&interner, "a");
    let b_idx = vocab_index(&interner, "b");
    let c_idx = vocab_index(&interner, "c");

    let ortho_a = Ortho::new().add(PayloadVal::try_from(a_idx).unwrap())[0].clone();
    let ortho_ab = ortho_a.add(PayloadVal::try_from(b_idx).unwrap())[0].clone();
    let ortho_ac = ortho_a.add(PayloadVal::try_from(c_idx).unwrap())[0].clone();

    // Use the same best_score derived from potentials above logic.
    let best_score = {
        let total_ab = interner.prefix_stats(&vec![a_idx, b_idx]).unwrap_or(0);
        let total_ac = interner.prefix_stats(&vec![a_idx, c_idx]).unwrap_or(0);
        let potential_ab = upper_bound_score(
            &[total_ab],
            ortho_a.volume(),
            ortho_a.fullness().saturating_add(1),
            ortho_a.dims().len(),
            total_ab,
        );
        let potential_ac = upper_bound_score(
            &[total_ac],
            ortho_a.volume(),
            ortho_a.fullness().saturating_add(1),
            ortho_a.dims().len(),
            total_ac,
        );
        if potential_ac.volume > potential_ab.volume {
            OrthoScore::optimistic_bound(potential_ac.volume.saturating_sub(1), usize::MAX)
        } else {
            OrthoScore::optimistic_bound(
                potential_ac.volume,
                potential_ac.fullness.saturating_sub(1),
            )
        }
    };

    assert!(
        bound_existing_ortho(
            &ortho_ab,
            &interner,
            best_score,
            Some(&vec![vec![a_idx, b_idx]])
        ),
        "prefix [a b] should be pruned from seeding"
    );
    assert!(
        !bound_existing_ortho(
            &ortho_ac,
            &interner,
            best_score,
            Some(&vec![vec![a_idx, b_idx]])
        ),
        "prefix [a c] should remain eligible for seeding"
    );
}
