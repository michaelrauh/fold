# Benchmarks for Fold

This directory contains comprehensive benchmarks for the Fold text processing system.

## Benchmark Suites

### End-to-End Benchmarks
**File**: `end_to_end_bench.rs`

Measures complete workflows from text input to ortho generation:
- `end_to_end_small_text`: Full workflow with small corpus
- `end_to_end_text_size`: Varying input sizes (50, 100, 200 sentences)
- `interner_from_text`: Interner construction performance

### Main Loop Benchmarks
**File**: `main_loop_bench.rs`

Focuses on the worker loop components:
- `worker_loop_single_iteration`: Complete processing iteration
- `interner_intersect_varying_requirements`: Intersection at different depths
- `ortho_child_generation`: Child ortho generation
- `seen_tracker_operations`: Deduplication performance
- `disk_backed_queue_operations`: Queue push/pop with disk spilling
- `interner_add_text`: Incremental interner updates
- `impacted_keys_detection`: Backtracking detection

### Component Benchmarks

#### Interner (`interner_bench.rs`)
- Intersection edge cases
- Vocabulary operations
- Construction complexity
- Prefix building
- FixedBitSet operations
- Version comparison
- Serialization

#### Ortho (`ortho_bench.rs`)
- Creation and addition
- ID computation
- Shape expansion

#### Spatial (`spatial_bench.rs`)
- Requirements calculation
- Base detection
- Expand up/over operations
- Caching effectiveness

#### Splitter (`splitter_bench.rs`)
- Vocabulary extraction
- Phrase generation
- Sentence splitting
- Text normalization
- Substring generation

#### DiskBackedQueue (`disk_queue_bench.rs`)
- Push operations (memory and disk)
- Pop operations (memory and disk)
- Mixed operations
- Persistence and reload
- Length tracking

#### SeenTracker (`seen_tracker_bench.rs`)
- Insert performance
- Contains/lookup performance
- Bloom filter effectiveness
- Comparison with HashSet
- Sharding overhead
- Disk operations

## Running Benchmarks

### Run All Benchmarks
```bash
cargo bench
```

### Run Specific Suite
```bash
cargo bench --bench end_to_end_bench
cargo bench --bench main_loop_bench
cargo bench --bench interner_bench
cargo bench --bench ortho_bench
cargo bench --bench spatial_bench
cargo bench --bench splitter_bench
cargo bench --bench disk_queue_bench
cargo bench --bench seen_tracker_bench
```

### Run Specific Benchmark
```bash
# Run only interner intersection benchmarks
cargo bench --bench interner_bench interner_intersect

# Run only ortho_add benchmarks
cargo bench --bench ortho_bench ortho_add
```

### Quick Test (No Full Run)
```bash
# Verify benchmarks compile and can run
cargo bench --bench ortho_bench -- --test
```

## Interpreting Results

### Criterion Output
Criterion provides detailed statistics:
```
ortho_new               time:   [33.309 ns 33.343 ns 33.389 ns]
                        ^^^^    ^^^^^^^^^ ^^^^^^^^^ ^^^^^^^^^
                        name    lower     estimate  upper bound
```

### HTML Reports
After running benchmarks, view detailed reports:
```bash
open target/criterion/report/index.html
```

Reports include:
- Timing distributions (violin plots)
- Iteration counts
- Outlier detection
- Comparison with previous runs

### Baseline Comparison
```bash
# Save current results as baseline
cargo bench -- --save-baseline before

# Make changes...

# Compare against baseline
cargo bench -- --baseline before
```

## Performance Targets

Based on analysis in `PERFORMANCE_ANALYSIS.md`:

### Critical Hotspots (Billions of Calls)
- `Ortho.id()`: ~50ns (target: <40ns with FxHash)
- `Ortho.add()`: ~50ns (target: <45ns with SmallVec)
- `Interner.intersect()`: Varies (target: 2-3x improvement with pooling)

### High Frequency (Millions of Calls)
- `SeenTracker.contains()`: <100ns for bloom hit
- `DiskBackedQueue.push()`: <1µs in-memory, <10µs with spill
- `Spatial operations`: <100ns (cached)

### One-Time Costs
- `Interner.from_text()`: Acceptable up to 10s for large texts
- `Splitter operations`: Acceptable up to 5s for large texts

## Continuous Performance Monitoring

### Automated Benchmarking
```bash
# Create benchmark baseline for CI
cargo bench -- --save-baseline ci-baseline

# In CI, compare against baseline and fail on regression
cargo bench -- --baseline ci-baseline
```

### Flamegraphs
For detailed profiling:
```bash
# Install flamegraph
cargo install flamegraph

# Generate flamegraph for end-to-end benchmark
cargo flamegraph --bench end_to_end_bench

# Open flamegraph.svg in browser
```

### Memory Profiling
```bash
# Install heaptrack
# On Ubuntu: sudo apt install heaptrack

# Profile memory usage
heaptrack cargo bench --bench end_to_end_bench

# Analyze results
heaptrack_gui heaptrack.cargo.*.gz
```

## Benchmark Development Guidelines

### Adding New Benchmarks

1. **Identify bottleneck**: Use profiling to find hotspots
2. **Create focused benchmark**: Test specific operation in isolation
3. **Add to appropriate suite**: Group by component
4. **Document expected performance**: Add targets to this file
5. **Validate**: Ensure benchmark reflects real usage

### Benchmark Best Practices

```rust
use criterion::{black_box, Criterion};

fn bench_my_operation(c: &mut Criterion) {
    // Setup (outside iteration)
    let data = setup_test_data();
    
    c.bench_function("my_operation", |b| {
        b.iter(|| {
            // Use black_box to prevent compiler optimization
            black_box(&data).my_operation(black_box(42))
        });
    });
}
```

### Common Pitfalls

1. **Dead code elimination**: Use `black_box()` for inputs and outputs
2. **Setup in benchmark**: Keep setup outside `iter()` closure
3. **Too fast benchmarks**: If <1ns, may be measuring overhead only
4. **Too slow benchmarks**: If >100ms, consider smaller workload
5. **Non-deterministic results**: Ensure reproducible setup

## See Also

- `PERFORMANCE_ANALYSIS.md`: Detailed performance analysis
- `PERFORMANCE_IMPROVEMENTS.md`: Prioritized improvement roadmap
- [Criterion.rs User Guide](https://bheisler.github.io/criterion.rs/book/)
