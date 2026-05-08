use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use fold::dfs_runner::{DfsRunner, SearchToggles};
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
    elapsed: Duration,
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
            name: "baseline_lifo_prune_both",
            toggles: SearchToggles::default(),
        },
        Variant {
            name: "lifo_no_completion_prune",
            toggles: SearchToggles {
                completion_pruning: false,
                ..SearchToggles::default()
            },
        },
        Variant {
            name: "lifo_no_node_prune",
            toggles: SearchToggles {
                node_pruning: false,
                ..SearchToggles::default()
            },
        },
        Variant {
            name: "lifo_no_pruning",
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
            "probe {} steps={} expanded={} pruned={} cpruned={} best_vol={} elapsed_ms={:.3}",
            variant.name,
            probe.steps_executed,
            probe.nodes_expanded,
            probe.nodes_pruned,
            probe.completions_pruned,
            probe.incumbent_volume,
            probe.elapsed.as_secs_f64() * 1000.0,
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
    let start = Instant::now();
    let mut steps = 0usize;
    for _ in 0..step_budget {
        if runner.is_finished() {
            break;
        }
        runner.step_with_toggles(interner, &toggles).unwrap();
        steps += 1;
    }
    ProbeOutcome {
        steps_executed: steps,
        nodes_expanded: runner.nodes_expanded(),
        nodes_pruned: runner.nodes_pruned(),
        completions_pruned: runner.completions_pruned(),
        incumbent_volume: runner.incumbent_score().volume as usize,
        elapsed: start.elapsed(),
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
        incumbent_volume: runner.incumbent_score().volume as usize,
    }
}

criterion_group!(benches, bench_dfs_ab);
criterion_main!(benches);
