use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId, Throughput};
use fold::generation_store::{Config, compact_landing, merge_unique, anti_join_orthos, RawStream, Run, UniqueRun};
use fold::ortho::Ortho;
use std::fs::{self, File};
use std::io::{Write, BufWriter};
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper to create a temporary test directory
fn setup_test_dir() -> TempDir {
    TempDir::new().expect("Failed to create temp dir")
}

/// Helper to create a raw stream with orthos for compact_landing testing
fn create_ortho_raw_stream(temp_dir: &PathBuf, bucket: usize, count: usize) -> RawStream {
    let landing_dir = temp_dir.join("landing").join(format!("b={:02}", bucket));
    fs::create_dir_all(&landing_dir).unwrap();
    
    // Create a drain file with orthos
    let drain_file = landing_dir.join("drain-0.log");
    let mut writer = BufWriter::new(File::create(&drain_file).unwrap());
    
    for i in 0..count {
        // Create orthos by starting with new() and adding values
        let mut ortho = Ortho::new();
        // Add values to build up the ortho
        ortho = ortho.add(i % 1000)[0].clone();
        if i % 100 < 50 {
            ortho = ortho.add((i / 2) % 1000)[0].clone();
        }
        
        let encoded = bincode::encode_to_vec(&ortho, bincode::config::standard()).unwrap();
        writer.write_all(&encoded).unwrap();
    }
    writer.flush().unwrap();
    
    RawStream::new(vec![drain_file])
}

/// Helper to create sorted ortho runs for merge_unique testing
fn create_sorted_ortho_runs(temp_dir: &PathBuf, num_runs: usize, items_per_run: usize) -> Vec<Run> {
    let runs_dir = temp_dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    
    let mut runs = Vec::new();
    
    for run_idx in 0..num_runs {
        let run_path = runs_dir.join(format!("run-{}.dat", run_idx));
        let mut writer = BufWriter::new(File::create(&run_path).unwrap());
        
        // Write sorted orthos with some overlap between runs
        for i in 0..items_per_run {
            let base_idx = run_idx * items_per_run / 2 + i;
            let mut ortho = Ortho::new();
            ortho = ortho.add(base_idx % 1000)[0].clone();
            
            let encoded = bincode::encode_to_vec(&ortho, bincode::config::standard()).unwrap();
            writer.write_all(&encoded).unwrap();
        }
        writer.flush().unwrap();
        
        runs.push(Run::new(run_path));
    }
    
    runs
}

/// Helper to create a unique run for anti-join testing
fn create_unique_ortho_run(temp_dir: &PathBuf, count: usize) -> UniqueRun {
    let runs_dir = temp_dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    
    let unique_path = runs_dir.join("unique.dat");
    let mut writer = BufWriter::new(File::create(&unique_path).unwrap());
    
    for i in 0..count {
        // Create orthos by starting with new() and adding values
        let mut ortho = Ortho::new();
        ortho = ortho.add(i)[0].clone();
        
        let encoded = bincode::encode_to_vec(&ortho, bincode::config::standard()).unwrap();
        writer.write_all(&encoded).unwrap();
    }
    writer.flush().unwrap();
    
    UniqueRun::new(unique_path)
}

/// Helper to create history orthos for anti-join testing
fn create_history_orthos(history_size: usize) -> Vec<Ortho> {
    let mut history = Vec::new();
    
    // Create history orthos for even numbers only (so odd numbers are new)
    for i in (0..history_size * 2).step_by(2) {
        let mut ortho = Ortho::new();
        ortho = ortho.add(i)[0].clone();
        history.push(ortho);
    }
    
    // Sort by ID for anti-join
    history.sort_by_key(|o| o.id());
    history
}

// ============================================================================
// BENCHMARK 1: Sort throughput vs RAM (compact_landing)
// ============================================================================

fn bench_sort_throughput_128mb(c: &mut Criterion) {
    // 128MB budget
    let cfg = Config {
        run_budget_bytes: 128 * 1024 * 1024,
        fan_in: 32,
        read_buf_bytes: 64 * 1024,
        allow_compaction: true,
    };
    
    // Create orthos for benchmarking (scaled down for reasonable test time)
    let count = 100_000;
    
    c.bench_function("compact_landing_128mb_100k_orthos", |b| {
        b.iter(|| {
            let temp_dir = setup_test_dir();
            let base_path = temp_dir.path().to_path_buf();
            fs::create_dir_all(base_path.join("runs")).unwrap();
            let raw = create_ortho_raw_stream(&base_path, 0, count);
            let runs = compact_landing(0, raw, &cfg, &base_path).unwrap();
            black_box(runs);
        });
    });
}

fn bench_sort_throughput_512mb(c: &mut Criterion) {
    // 512MB budget
    let cfg = Config {
        run_budget_bytes: 512 * 1024 * 1024,
        fan_in: 64,
        read_buf_bytes: 64 * 1024,
        allow_compaction: true,
    };
    
    // Create orthos for benchmarking
    let count = 100_000;
    
    c.bench_function("compact_landing_512mb_100k_orthos", |b| {
        b.iter(|| {
            let temp_dir = setup_test_dir();
            let base_path = temp_dir.path().to_path_buf();
            fs::create_dir_all(base_path.join("runs")).unwrap();
            let raw = create_ortho_raw_stream(&base_path, 0, count);
            let runs = compact_landing(0, raw, &cfg, &base_path).unwrap();
            black_box(runs);
        });
    });
}

fn bench_sort_throughput_2gb(c: &mut Criterion) {
    // 2GB budget
    let cfg = Config {
        run_budget_bytes: 2 * 1024 * 1024 * 1024,
        fan_in: 128,
        read_buf_bytes: 64 * 1024,
        allow_compaction: true,
    };
    
    // Create orthos for benchmarking
    let count = 100_000;
    
    c.bench_function("compact_landing_2gb_100k_orthos", |b| {
        b.iter(|| {
            let temp_dir = setup_test_dir();
            let base_path = temp_dir.path().to_path_buf();
            fs::create_dir_all(base_path.join("runs")).unwrap();
            let raw = create_ortho_raw_stream(&base_path, 0, count);
            let runs = compact_landing(0, raw, &cfg, &base_path).unwrap();
            black_box(runs);
        });
    });
}

// ============================================================================
// BENCHMARK 2: Anti-join vs history size
// ============================================================================

fn bench_anti_join_history_1k(c: &mut Criterion) {
    let temp_dir = setup_test_dir();
    let base_path = temp_dir.path().to_path_buf();
    fs::create_dir_all(base_path.join("runs")).unwrap();
    
    // Create unique run with 10k orthos
    let unique_run = create_unique_ortho_run(&base_path, 10_000);
    
    // Create history with 1k orthos (even numbers)
    let history = create_history_orthos(1_000);
    
    c.bench_function("anti_join_history_1k", |b| {
        b.iter(|| {
            let history_iter = history.clone().into_iter().map(Ok);
            let (work, _run, _count) = anti_join_orthos(
                unique_run.clone(),
                history_iter,
                &base_path
            ).unwrap();
            black_box(work);
        });
    });
}

fn bench_anti_join_history_10k(c: &mut Criterion) {
    let temp_dir = setup_test_dir();
    let base_path = temp_dir.path().to_path_buf();
    fs::create_dir_all(base_path.join("runs")).unwrap();
    
    // Create unique run with 100k orthos
    let unique_run = create_unique_ortho_run(&base_path, 100_000);
    
    // Create history with 10k orthos (even numbers)
    let history = create_history_orthos(10_000);
    
    c.bench_function("anti_join_history_10k", |b| {
        b.iter(|| {
            let history_iter = history.clone().into_iter().map(Ok);
            let (work, _run, _count) = anti_join_orthos(
                unique_run.clone(),
                history_iter,
                &base_path
            ).unwrap();
            black_box(work);
        });
    });
}

fn bench_anti_join_history_100k(c: &mut Criterion) {
    let temp_dir = setup_test_dir();
    let base_path = temp_dir.path().to_path_buf();
    fs::create_dir_all(base_path.join("runs")).unwrap();
    
    // Create unique run with 1M orthos
    let unique_run = create_unique_ortho_run(&base_path, 1_000_000);
    
    // Create history with 100k orthos (even numbers)
    let history = create_history_orthos(100_000);
    
    c.bench_function("anti_join_history_100k", |b| {
        b.iter(|| {
            let history_iter = history.clone().into_iter().map(Ok);
            let (work, _run, _count) = anti_join_orthos(
                unique_run.clone(),
                history_iter,
                &base_path
            ).unwrap();
            black_box(work);
        });
    });
}

// ============================================================================
// BENCHMARK 3: Full generation with duplicates
// ============================================================================

fn bench_full_generation_with_duplicates(c: &mut Criterion) {
    let cfg = Config {
        run_budget_bytes: 256 * 1024 * 1024, // 256MB
        fan_in: 32,
        read_buf_bytes: 64 * 1024,
        allow_compaction: true,
    };
    
    c.bench_function("full_generation_1m_ints_50pct_dupes", |b| {
        b.iter(|| {
            // Create fresh temp dir for each iteration
            let temp_dir = setup_test_dir();
            let base_path = temp_dir.path().to_path_buf();
            fs::create_dir_all(base_path.join("runs")).unwrap();
            
            // Step 1: Create raw stream with orthos, 50% duplicates
            let landing_dir = base_path.join("landing").join("b=00");
            fs::create_dir_all(&landing_dir).unwrap();
            
            let drain_file = landing_dir.join("drain-test.log");
            let mut writer = BufWriter::new(File::create(&drain_file).unwrap());
            
            // Create 50k orthos with 50% duplicates (reduced from 1M for reasonable bench time)
            for i in 0..50_000 {
                // Every other value is a duplicate
                let base_idx = i / 2;
                let mut ortho = Ortho::new();
                ortho = ortho.add(base_idx % 1000)[0].clone();
                
                let encoded = bincode::encode_to_vec(&ortho, bincode::config::standard()).unwrap();
                writer.write_all(&encoded).unwrap();
            }
            writer.flush().unwrap();
            
            let raw = RawStream::new(vec![drain_file]);
            
            // Step 2: Compact landing into sorted runs
            let runs = compact_landing(0, raw, &cfg, &base_path).unwrap();
            
            // Step 3: Merge runs into unique run
            let unique_run = merge_unique(runs, &cfg, &base_path).unwrap();
            
            // Step 4: Anti-join against empty history (all new)
            let empty_history = std::iter::empty();
            
            let (work, _run, _count) = anti_join_orthos(
                unique_run,
                empty_history,
                &base_path
            ).unwrap();
            
            black_box(work);
            // No need to cleanup - temp_dir drops automatically
        });
    });
}

fn bench_merge_unique_varying_runs(c: &mut Criterion) {
    let mut group = c.benchmark_group("merge_unique_fan_in");
    
    let cfg = Config {
        run_budget_bytes: 512 * 1024 * 1024,
        fan_in: 32,
        read_buf_bytes: 64 * 1024,
        allow_compaction: true,
    };
    
    for num_runs in [4, 8, 16, 32, 64].iter() {
        group.throughput(Throughput::Elements((*num_runs * 10_000) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(num_runs),
            num_runs,
            |b, _| {
                b.iter(|| {
                    let temp_dir = setup_test_dir();
                    let base_path = temp_dir.path().to_path_buf();
                    // Create runs with 10k items each
                    let runs = create_sorted_ortho_runs(&base_path, *num_runs, 10_000);
                    let unique = merge_unique(runs, &cfg, &base_path).unwrap();
                    black_box(unique);
                });
            }
        );
    }
    
    group.finish();
}

criterion_group!(
    sort_throughput,
    bench_sort_throughput_128mb,
    bench_sort_throughput_512mb,
    bench_sort_throughput_2gb,
);

criterion_group!(
    anti_join_benches,
    bench_anti_join_history_1k,
    bench_anti_join_history_10k,
    bench_anti_join_history_100k,
);

criterion_group!(
    full_generation,
    bench_full_generation_with_duplicates,
    bench_merge_unique_varying_runs,
);

criterion_main!(sort_throughput, anti_join_benches, full_generation);
