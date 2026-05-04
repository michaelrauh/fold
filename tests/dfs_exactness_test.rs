use fixedbitset::FixedBitSet;
use fold::{
    completion_pruning::{CompletionContext, completion_upper_bound_ctx},
    dfs_runner::DfsRunner,
    interner::Interner,
    ortho::{Ortho, PayloadVal},
};

fn exhaustive_best(interner: &Interner) -> Ortho {
    fn walk(interner: &Interner, ortho: &Ortho, best: &mut Ortho) {
        if ortho.score() > best.score() {
            *best = ortho.clone();
        }

        let mut ctx = CompletionContext::from_ortho(ortho);
        let mut bits = FixedBitSet::with_capacity(interner.vocab_size());
        bits.grow(interner.vocab_size());
        interner.intersect_into(ctx.required_usize(), ctx.forbidden_usize(), &mut bits);

        for completion in bits.ones() {
            if completion_upper_bound_ctx(&mut ctx, completion, interner).is_none() {
                continue;
            }
            let value = PayloadVal::try_from(completion).unwrap();
            for child in ortho.add(value) {
                walk(interner, &child, best);
            }
        }
    }

    let root = Ortho::new();
    let mut best = root.clone();
    walk(interner, &root, &mut best);
    best
}

#[test]
fn dfs_matches_exhaustive_best_score_on_small_corpus() {
    let interner = Interner::from_text("a b c. a d e.");
    let exhaustive = exhaustive_best(&interner);

    let mut runner = DfsRunner::new();
    while !runner.is_finished() {
        runner.step(&interner).unwrap();
    }

    assert_eq!(runner.incumbent_score(), exhaustive.score());
}

#[test]
fn runner_snapshot_frontier_metrics_stay_consistent() {
    let interner = Interner::from_text("a b c. a d e.");
    let mut runner = DfsRunner::new();

    for _ in 0..5 {
        if runner.is_finished() {
            break;
        }
        runner.step(&interner).unwrap();
        let snapshot = runner.snapshot();
        assert_eq!(
            snapshot.open_siblings_total,
            snapshot.open_siblings_by_depth.iter().sum::<u64>()
        );
        assert_eq!(
            snapshot.path_progress_by_depth.len(),
            snapshot.current_depth
        );
        if snapshot.open_siblings_total == 0 {
            assert!(snapshot.frontier_max_bound.is_none());
        } else {
            assert!(snapshot.frontier_max_bound.is_some());
        }
    }

    while !runner.is_finished() {
        runner.step(&interner).unwrap();
    }
    let final_snapshot = runner.snapshot();
    assert_eq!(final_snapshot.open_siblings_total, 0);
    assert!(final_snapshot.frontier_max_bound.is_none());
    assert_eq!(
        final_snapshot.path_progress_by_depth.len(),
        final_snapshot.current_depth
    );
    assert_eq!(
        final_snapshot.open_siblings_total,
        final_snapshot.open_siblings_by_depth.iter().sum::<u64>()
    );
}
