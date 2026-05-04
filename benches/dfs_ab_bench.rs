use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use fold::dfs_runner::{BranchOrdering, DfsRunner, SearchProfile, SearchToggles};
use fold::interner::Interner;
use std::fs;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
struct Variant {
    name: &'static str,
    toggles: SearchToggles,
}

#[derive(Clone, Copy, Debug)]
struct ProbeOutcome {
    steps_executed: usize,
    nodes_expanded: u64,
    nodes_pruned: u64,
    completions_pruned: u64,
    incumbent_volume: usize,
    total_step_ns: u128,
    existing_bound_ns: u128,
    intersect_ns: u128,
    child_generation_ns: u128,
    reorder_ns: u128,
    node_prune_ns: u128,
    completion_prune_ns: u128,
    completion_bound_ns: u128,
}

#[derive(Clone, Copy, Debug)]
struct BudgetOutcome {
    elapsed: Duration,
    steps_executed: usize,
    nodes_expanded: u64,
    incumbent_volume: usize,
}

fn bench_dfs_ab(c: &mut Criterion) {
    let text = fs::read_to_string("e.txt").expect("failed to read e.txt from repository root");
    let interner = Interner::from_text(&text);
    let step_budget = std::env::var("FOLD_BENCH_STEPS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20_000);

    let variants = [
        Variant {
            name: "baseline_bestfirst_prune_both",
            toggles: SearchToggles::default(),
        },
        Variant {
            name: "branch_insertion_prune_both",
            toggles: SearchToggles {
                branch_ordering: BranchOrdering::Insertion,
                ..SearchToggles::default()
            },
        },
        Variant {
            name: "branch_worstfirst_prune_both",
            toggles: SearchToggles {
                branch_ordering: BranchOrdering::WorstFirst,
                ..SearchToggles::default()
            },
        },
        Variant {
            name: "bestfirst_no_completion_prune",
            toggles: SearchToggles {
                completion_pruning: false,
                ..SearchToggles::default()
            },
        },
        Variant {
            name: "bestfirst_no_node_prune",
            toggles: SearchToggles {
                node_pruning: false,
                ..SearchToggles::default()
            },
        },
        Variant {
            name: "bestfirst_no_pruning",
            toggles: SearchToggles {
                node_pruning: false,
                completion_pruning: false,
                ..SearchToggles::default()
            },
        },
        Variant {
            name: "no_bounds_no_pruning",
            toggles: SearchToggles {
                node_pruning: false,
                completion_pruning: false,
                branch_ordering: BranchOrdering::Insertion,
                compute_bounds: false,
            },
        },
    ];

    let filter = std::env::var("FOLD_BENCH_VARIANT").ok();
    let variants: Vec<Variant> = variants
        .into_iter()
        .filter(|variant| {
            filter
                .as_ref()
                .map(|needle| variant.name.contains(needle))
                .unwrap_or(true)
        })
        .collect();
    if variants.is_empty() {
        panic!("FOLD_BENCH_VARIANT filter matched no benchmark variants");
    }

    let mut group = c.benchmark_group("dfs_ab");
    group.sample_size(10);
    let seconds_budget = std::env::var("FOLD_BENCH_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5);

    for variant in variants {
        let probe = run_steps(&interner, variant.toggles, step_budget);
        eprintln!(
            "probe {} steps={} expanded={} pruned={} cpruned={} best_vol={} total_ms={:.3} existing_bound_ms={:.3} intersect_ms={:.3} child_gen_ms={:.3} reorder_ms={:.3} node_prune_ms={:.3} completion_prune_ms={:.3} completion_bound_ms={:.3}",
            variant.name,
            probe.steps_executed,
            probe.nodes_expanded,
            probe.nodes_pruned,
            probe.completions_pruned,
            probe.incumbent_volume,
            probe.total_step_ns as f64 / 1_000_000.0,
            probe.existing_bound_ns as f64 / 1_000_000.0,
            probe.intersect_ns as f64 / 1_000_000.0,
            probe.child_generation_ns as f64 / 1_000_000.0,
            probe.reorder_ns as f64 / 1_000_000.0,
            probe.node_prune_ns as f64 / 1_000_000.0,
            probe.completion_prune_ns as f64 / 1_000_000.0,
            probe.completion_bound_ns as f64 / 1_000_000.0,
        );
        let total = probe.total_step_ns.max(1) as f64;
        eprintln!(
            "probe_pct {} existing_bound={:.2}% intersect={:.2}% child_gen={:.2}% reorder={:.2}% node_prune={:.2}% completion_prune={:.2}% completion_bound={:.2}%",
            variant.name,
            100.0 * probe.existing_bound_ns as f64 / total,
            100.0 * probe.intersect_ns as f64 / total,
            100.0 * probe.child_generation_ns as f64 / total,
            100.0 * probe.reorder_ns as f64 / total,
            100.0 * probe.node_prune_ns as f64 / total,
            100.0 * probe.completion_prune_ns as f64 / total,
            100.0 * probe.completion_bound_ns as f64 / total,
        );

        let budget = run_for_duration(
            &interner,
            variant.toggles,
            Duration::from_secs(seconds_budget),
        );
        eprintln!(
            "probe_budget {} seconds={} elapsed_ms={:.2} steps={} expanded={} best_vol={}",
            variant.name,
            seconds_budget,
            budget.elapsed.as_secs_f64() * 1000.0,
            budget.steps_executed,
            budget.nodes_expanded,
            budget.incumbent_volume,
        );

        group.bench_with_input(BenchmarkId::new("steps", variant.name), &variant, |b, v| {
            b.iter(|| {
                let outcome = run_steps(&interner, v.toggles, step_budget);
                black_box(outcome);
            })
        });
    }

    group.finish();
}

fn run_steps(interner: &Interner, toggles: SearchToggles, step_budget: usize) -> ProbeOutcome {
    let mut runner = DfsRunner::new();
    let mut profile = SearchProfile::default();
    let mut steps = 0usize;
    for _ in 0..step_budget {
        if runner.is_finished() {
            break;
        }
        runner
            .step_with_toggles_and_profile(interner, &toggles, &mut profile)
            .unwrap();
        steps += 1;
    }
    ProbeOutcome {
        steps_executed: steps,
        nodes_expanded: runner.nodes_expanded(),
        nodes_pruned: runner.nodes_pruned(),
        completions_pruned: runner.completions_pruned(),
        incumbent_volume: runner.incumbent_score().volume,
        total_step_ns: profile.total_step_ns,
        existing_bound_ns: profile.existing_bound_ns,
        intersect_ns: profile.intersect_ns,
        child_generation_ns: profile.child_generation_ns,
        reorder_ns: profile.reorder_ns,
        node_prune_ns: profile.node_prune_ns,
        completion_prune_ns: profile.completion_prune_ns,
        completion_bound_ns: profile.completion_bound_ns,
    }
}

fn run_for_duration(
    interner: &Interner,
    toggles: SearchToggles,
    budget: Duration,
) -> BudgetOutcome {
    let start = Instant::now();
    let mut runner = DfsRunner::new();
    let mut steps = 0usize;
    while start.elapsed() < budget {
        if runner.is_finished() {
            break;
        }
        runner.step_with_toggles(interner, &toggles).unwrap();
        steps += 1;
    }
    BudgetOutcome {
        elapsed: start.elapsed(),
        steps_executed: steps,
        nodes_expanded: runner.nodes_expanded(),
        incumbent_volume: runner.incumbent_score().volume,
    }
}

criterion_group!(benches, bench_dfs_ab);
criterion_main!(benches);
