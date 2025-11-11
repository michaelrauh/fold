# Performance Improvements Roadmap for Fold

This document outlines specific, actionable performance improvements for the Fold system. Each improvement is prioritized, estimated for impact, and includes implementation details. These improvements are **NOT YET IMPLEMENTED** - they are recommendations based on code analysis and benchmark design.

## Implementation Priority Matrix

| Priority | Component | Improvement | Est. Impact | Implementation Effort | Risk |
|----------|-----------|-------------|-------------|----------------------|------|
| P0 | Interner | Use FxHash instead of DefaultHasher | 20-30% | Low | Low |
| P0 | Ortho | Replace DefaultHasher with FxHash | 20-30% | Low | Low |
| P1 | Interner | Pool FixedBitSets for intersect | 20-30% | Medium | Low |
| P1 | Ortho | Use SmallVec for payload | 10-15% | Medium | Low |
| P1 | SeenTracker | Optimize bloom filter parameters | 10-20% | Low | Medium |
| P2 | DiskBackedQueue | Increase buffer sizes | 30-50% I/O reduction | Low | Low |
| P2 | Interner | Parallel prefix building | 2-4x build time | Medium | Low |
| P2 | Splitter | Parallel processing with rayon | 2-4x | Medium | Low |
| P3 | DiskBackedQueue | Batch serialization | 20-30% | High | Medium |
| P3 | Interner | SIMD bitset operations | 2-3x | High | High |
| P3 | SeenTracker | Memory-mapped files | 20-40% disk ops | High | Medium |

## Detailed Improvements

### P0: Quick Wins (Minimal Code Changes, High Impact)

#### P0.1: Replace DefaultHasher with FxHash

**Current State**: Both `Ortho::compute_id()` and various HashMap usage use `DefaultHasher`

**Problem**: DefaultHasher is cryptographically secure but slow. For deduplication, we need speed, not security.

**Solution**:
```rust
// In src/ortho.rs
use rustc_hash::FxHasher; // Already a dependency

fn compute_id(version: usize, dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
    if payload.iter().all(|x| x.is_none()) {
        let mut hasher = FxHasher::default(); // Changed from DefaultHasher
        version.hash(&mut hasher);
        (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
    } else {
        let mut hasher = FxHasher::default(); // Changed from DefaultHasher
        dims.hash(&mut hasher);
        payload.hash(&mut hasher);
        (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
    }
}
```

**Expected Impact**: 20-30% improvement in ID computation
**Effort**: 2 line changes + import
**Risk**: Low - FxHash is widely used, no algorithmic changes

**Validation**:
- Run `cargo bench --bench ortho_bench` before and after
- Verify `ortho_id` benchmark improvement
- Run full test suite to ensure correctness

---

#### P0.2: Use FxHashMap Consistently Throughout

**Current State**: Some modules use `std::collections::HashMap`, others use `FxHashMap`

**Problem**: Standard HashMap is slower than FxHashMap for integer keys

**Solution**:
```rust
// In src/interner.rs - already uses HashMap
use rustc_hash::FxHashMap;

#[derive(Clone)]
pub struct Interner {
    version: usize,
    vocabulary: Vec<String>,
    prefix_to_completions: FxHashMap<Vec<usize>, FixedBitSet>, // Changed
}

// In src/splitter.rs - uses BTreeSet, consider HashSet where order doesn't matter
// Only change where order is not needed
```

**Expected Impact**: 10-20% improvement in interner operations
**Effort**: Update imports and type declarations
**Risk**: Low - FxHashMap is a drop-in replacement

**Validation**:
- Run `cargo bench --bench interner_bench`
- Compare `interner_from_text` and `interner_intersect` timings
- Ensure all tests pass

---

### P1: High Impact Optimizations (Moderate Effort)

#### P1.1: Pool FixedBitSets for Interner.intersect()

**Current State**: Each `intersect()` call clones FixedBitSets multiple times

**Problem**: 
```rust
// Current code in interner.rs (simplified)
let mut result = required_bits.clone(); // Clone 1
for prefix in required {
    let bits = self.get_required_bits(prefix); // Clone 2
    result.intersect_with(&bits); // More allocations
}
```

**Solution**:
```rust
use std::cell::RefCell;

thread_local! {
    static BITSET_POOL: RefCell<Vec<FixedBitSet>> = RefCell::new(Vec::new());
}

fn get_pooled_bitset(capacity: usize) -> FixedBitSet {
    BITSET_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        pool.pop().unwrap_or_else(|| FixedBitSet::with_capacity(capacity))
    })
}

fn return_to_pool(mut bitset: FixedBitSet) {
    BITSET_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < 100 { // Limit pool size
            bitset.clear();
            pool.push(bitset);
        }
    });
}

// In intersect() method
pub fn intersect(&self, required: &[Vec<usize>], forbidden: &[usize]) -> Vec<usize> {
    let vocab_len = self.vocabulary.len();
    let mut result = get_pooled_bitset(vocab_len);
    result.grow(vocab_len);
    result.set_range(.., true); // All true initially
    
    // ... rest of logic using pooled bitsets ...
    
    let indices = result.ones().collect();
    return_to_pool(result); // Return to pool
    indices
}
```

**Expected Impact**: 20-30% improvement in `intersect()`
**Effort**: Medium - requires careful lifetime management
**Risk**: Low - isolated to intersect method

**Validation**:
- Run `cargo bench --bench interner_bench`
- Focus on `interner_intersect_cases` benchmarks
- Verify no memory leaks with valgrind

---

#### P1.2: Use SmallVec for Ortho Payload

**Current State**: `payload: Vec<Option<usize>>` always heap allocates

**Problem**: Most orthos are small (2x2 = 4 elements initially), heap allocation overhead is significant

**Solution**:
```rust
// In Cargo.toml - smallvec is already a dependency
// In src/ortho.rs
use smallvec::SmallVec;

#[derive(PartialEq, Debug, Clone, Encode, Decode)]
pub struct Ortho {
    version: usize,
    dims: Vec<usize>, // Keep Vec, dims are small
    payload: SmallVec<[Option<usize>; 16]>, // Inline up to 16 elements (128 bytes)
}

// Update all Vec<Option<usize>> references to SmallVec<[Option<usize>; 16]>
```

**Tuning**: Profile different inline sizes (8, 16, 32) based on actual ortho size distribution

**Expected Impact**: 10-15% improvement in ortho operations
**Effort**: Medium - need to update bincode serialization
**Risk**: Low - SmallVec is well-tested

**Validation**:
- Run `cargo bench --bench ortho_bench`
- Verify all `ortho_add_*` benchmarks improve
- Test serialization with `cargo test`

---

#### P1.3: Optimize SeenTracker Bloom Filter Parameters

**Current State**: Fixed bloom filter FP rate of 0.01

**Problem**: Too conservative - wastes memory. Slight increase in FP rate could save significant memory with minor performance impact.

**Solution**:
```rust
// In src/seen_tracker.rs
pub fn with_config(bloom_capacity: usize, num_shards: usize, max_shards_in_memory: usize) -> Self {
    // Test with 0.05 FP rate instead of 0.01
    let false_positive_rate = 0.05; // Changed from 0.01
    let bloom = Bloom::new_for_fp_rate(bloom_capacity, false_positive_rate);
    // ...
}
```

**Trade-off Analysis**:
- 0.01 FP: ~9.6 bits per element
- 0.05 FP: ~6.2 bits per element
- Memory savings: ~35%
- Extra hashmap checks: ~5% more, but hashmap checks are fast with sharding

**Expected Impact**: 35% memory reduction, 5% performance loss (net positive)
**Effort**: Low - parameter tuning
**Risk**: Medium - requires validation of FP rate impact

**Validation**:
- Run `cargo bench --bench seen_tracker_bench`
- Compare `tracker_bloom_effectiveness` results
- Monitor `tracker_vs_hashset` performance
- Test with billion-scale workload

---

### P2: High-Value Optimizations (Higher Effort)

#### P2.1: Increase DiskBackedQueue Buffer Sizes

**Current State**: Default buffer size from `MemoryConfig`, typically conservative

**Problem**: Too many disk spills, I/O overhead dominates when queue is large

**Solution**:
```rust
// In src/memory_config.rs
impl MemoryConfig {
    pub fn calculate(interner_bytes: usize, result_count: usize) -> Self {
        let system_mem = get_available_memory();
        
        // More aggressive buffer sizing
        let queue_buffer_size = if system_mem > 16_000_000_000 {
            5000 // Up from default
        } else if system_mem > 8_000_000_000 {
            2000 // Up from default
        } else {
            1000 // Keep default for low memory
        };
        
        // ... rest of calculation
    }
}
```

**Expected Impact**: 30-50% reduction in disk I/O operations
**Effort**: Low - configuration change
**Risk**: Low - graceful degradation if memory insufficient

**Validation**:
- Run `cargo bench --bench disk_queue_bench`
- Focus on `queue_push_spill` and `queue_pop_disk`
- Monitor memory usage with `sysinfo`

---

#### P2.2: Parallel Interner Prefix Building

**Current State**: Serial iteration over phrases to build `prefix_to_completions`

**Problem**: Single-threaded, CPU bottleneck for large texts

**Solution**:
```rust
// In src/interner.rs
use rayon::prelude::*; // Already a dependency

fn build_prefix_to_completions(
    phrases: &[Vec<String>],
    vocabulary: &[String],
    vocab_len: usize,
    existing: Option<&HashMap<Vec<usize>, FixedBitSet>>,
) -> HashMap<Vec<usize>, FixedBitSet> {
    use std::sync::Mutex;
    
    // Thread-safe accumulator
    let prefix_map = Mutex::new(HashMap::new());
    
    // Parallel iteration over phrases
    phrases.par_iter().for_each(|phrase| {
        let indices: Vec<usize> = phrase.iter()
            .map(|word| vocabulary.iter().position(|v| v == word).unwrap())
            .collect();
        
        // Generate all prefixes for this phrase
        let mut local_prefixes = HashMap::new();
        for i in 0..indices.len() {
            let prefix = indices[..i].to_vec();
            let completion = indices[i];
            
            local_prefixes.entry(prefix)
                .or_insert_with(|| FixedBitSet::with_capacity(vocab_len))
                .insert(completion);
        }
        
        // Merge into global map
        let mut map = prefix_map.lock().unwrap();
        for (prefix, completions) in local_prefixes {
            map.entry(prefix)
                .or_insert_with(|| FixedBitSet::with_capacity(vocab_len))
                .union_with(&completions);
        }
    });
    
    prefix_map.into_inner().unwrap()
}
```

**Expected Impact**: 2-4x faster interner construction on multi-core
**Effort**: Medium - requires careful synchronization
**Risk**: Low - rayon handles thread safety

**Validation**:
- Run `cargo bench --bench interner_bench`
- Compare `interner_from_text_complexity` timings
- Verify correctness with tests

---

#### P2.3: Parallel Splitter Processing

**Current State**: Serial sentence processing

**Problem**: Independent sentences can be processed in parallel

**Solution**:
```rust
// In src/splitter.rs
use rayon::prelude::*;

pub fn vocabulary(&self, text: &str) -> Vec<String> {
    use std::sync::Mutex;
    
    let vocab_set = Mutex::new(BTreeSet::new());
    
    self.split_into_sentences(text)
        .par_iter() // Parallel iteration
        .for_each(|sentence| {
            let words = self.clean_sentence(sentence);
            let mut set = vocab_set.lock().unwrap();
            for word in words {
                set.insert(word);
            }
        });
    
    vocab_set.into_inner().unwrap().into_iter().collect()
}

pub fn phrases(&self, text: &str) -> Vec<Vec<String>> {
    self.split_into_sentences(text)
        .par_iter() // Parallel iteration
        .flat_map(|sentence| {
            let words = self.clean_sentence(sentence);
            if words.len() >= 2 {
                self.generate_substrings(&words)
            } else {
                vec![]
            }
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
```

**Expected Impact**: 2-4x faster text processing
**Effort**: Medium - requires understanding of rayon patterns
**Risk**: Low - sentences are independent

**Validation**:
- Run `cargo bench --bench splitter_bench`
- All `splitter_*` benchmarks should improve
- Verify output matches serial version

---

### P3: Advanced Optimizations (High Effort, High Risk)

#### P3.1: SIMD Bitset Operations

**Current State**: FixedBitSet uses standard bit operations

**Problem**: Can leverage SIMD instructions for parallel bit operations

**Solution**:
```rust
// Create custom SIMD bitset
// In new file: src/simd_bitset.rs
#![feature(portable_simd)]
use std::simd::*;

pub struct SimdBitset {
    data: Vec<u64x8>, // 512 bits per chunk
    len: usize,
}

impl SimdBitset {
    pub fn intersect_with(&mut self, other: &Self) {
        for i in 0..self.data.len() {
            self.data[i] &= other.data[i]; // SIMD AND
        }
    }
    
    pub fn union_with(&mut self, other: &Self) {
        for i in 0..self.data.len() {
            self.data[i] |= other.data[i]; // SIMD OR
        }
    }
    
    // ... other operations
}
```

**Expected Impact**: 2-3x improvement in bitset operations
**Effort**: High - requires nightly Rust, extensive testing
**Risk**: High - complex implementation, portability concerns

**Validation**:
- Comprehensive benchmarking against FixedBitSet
- Property-based testing for correctness
- Test on multiple architectures

**Alternative**: Use existing SIMD libraries like `bitvec` or `simd-bitset` crates

---

#### P3.2: Batch DiskBackedQueue Serialization

**Current State**: Each ortho serialized individually

**Problem**: Many small I/O operations, syscall overhead

**Solution**:
```rust
// In src/disk_backed_queue.rs
impl DiskBackedQueue {
    fn spill_to_disk(&mut self) -> Result<(), FoldError> {
        let chunk_size = 100; // Serialize 100 orthos per write
        let chunks: Vec<Vec<Ortho>> = self.buffer
            .chunks(chunk_size)
            .map(|c| c.to_vec())
            .collect();
        
        let file_path = self.disk_path.join(format!("queue_{:08}.bin", self.disk_file_counter));
        let file = File::create(&file_path)?;
        let mut writer = BufWriter::new(file);
        
        for chunk in chunks {
            // Serialize entire chunk in one write
            bincode::encode_into_std_write(&chunk, &mut writer, bincode::config::standard())?;
        }
        
        writer.flush()?;
        self.disk_file_counter += 1;
        Ok(())
    }
}
```

**Expected Impact**: 20-30% improvement in queue I/O
**Effort**: High - requires rework of serialization format
**Risk**: Medium - need to maintain backward compatibility

**Validation**:
- Run `cargo bench --bench disk_queue_bench`
- Verify `queue_push_spill` improves significantly
- Test checkpoint compatibility

---

#### P3.3: Memory-Mapped Files for SeenTracker

**Current State**: Explicit read/write for disk-backed shards

**Problem**: Lots of syscalls, manual memory management

**Solution**:
```rust
// In src/seen_tracker.rs
use memmap2::MmapMut; // Add dependency

struct MmappedShard {
    id: usize,
    mmap: MmapMut,
    len: usize,
    dirty: bool,
}

impl MmappedShard {
    fn new(path: &Path, capacity: usize) -> Result<Self, FoldError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;
        
        file.set_len((capacity * 8) as u64)?; // usize entries
        
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        
        Ok(Self {
            id: 0,
            mmap,
            len: 0,
            dirty: false,
        })
    }
    
    fn insert(&mut self, id: usize) {
        // Write directly to mmap
        let offset = self.len * 8;
        let bytes = id.to_le_bytes();
        self.mmap[offset..offset+8].copy_from_slice(&bytes);
        self.len += 1;
        self.dirty = true;
    }
}
```

**Expected Impact**: 20-40% improvement in disk-backed operations
**Effort**: High - requires careful memory management
**Risk**: Medium - platform-specific behavior, crash safety

**Validation**:
- Run `cargo bench --bench seen_tracker_bench`
- Focus on `tracker_disk_ops`
- Test crash recovery scenarios

---

## Implementation Order

### Phase 1: Low-Hanging Fruit (Week 1)
1. P0.1: FxHash for Ortho.id() ✓
2. P0.2: FxHashMap throughout ✓
3. P1.3: Tune bloom filter parameters ✓
4. P2.1: Increase buffer sizes ✓

**Expected cumulative improvement**: 40-60%

### Phase 2: Core Optimizations (Week 2-3)
1. P1.1: Pool FixedBitSets ✓
2. P1.2: SmallVec for payload ✓
3. P2.2: Parallel interner building ✓
4. P2.3: Parallel splitter ✓

**Expected cumulative improvement**: 2-3x total (on multi-core)

### Phase 3: Advanced Optimizations (Week 4+)
1. P3.1: SIMD bitset operations (if needed)
2. P3.2: Batch serialization (if I/O bound)
3. P3.3: Memory-mapped files (if disk-bound)

**Expected cumulative improvement**: 3-5x total

## Validation Strategy

After each phase:

1. **Benchmark validation**: Run full benchmark suite
   ```bash
   cargo bench > phase_N_results.txt
   git diff baseline.txt phase_N_results.txt
   ```

2. **Correctness validation**: Run full test suite
   ```bash
   cargo test
   ```

3. **Scale testing**: Test with progressively larger workloads
   - 1K orthos
   - 100K orthos
   - 1M orthos
   - 10M orthos (if feasible)

4. **Profiling**: Generate flamegraphs to validate hotspot elimination
   ```bash
   cargo flamegraph --bench end_to_end_bench
   ```

## Success Metrics

| Metric | Baseline | Phase 1 Target | Phase 2 Target | Phase 3 Target |
|--------|----------|---------------|---------------|---------------|
| Ortho.id() speed | 100% | 130% | 130% | 130% |
| Interner.intersect() speed | 100% | 120% | 150% | 300% |
| Ortho.add() speed | 100% | 110% | 125% | 125% |
| SeenTracker ops | 100% | 110% | 110% | 150% |
| DiskQueue ops | 100% | 150% | 150% | 180% |
| End-to-end throughput | 100% | 160% | 300% | 500% |
| Memory usage | 100% | 70% | 70% | 50% |

## Risk Mitigation

### High-Risk Changes
For P3 optimizations:
1. **Feature flags**: Use cargo features to toggle new implementations
2. **A/B testing**: Run both old and new side-by-side, compare results
3. **Gradual rollout**: Deploy to subset of workloads first

### Rollback Plan
```bash
# Tag before each phase
git tag phase-1-baseline
git tag phase-2-baseline
git tag phase-3-baseline

# Rollback if needed
git revert <commit-range>
```

## Monitoring Post-Implementation

After deploying optimizations, monitor:
1. **Throughput**: Orthos processed per second
2. **Memory usage**: Peak RSS, disk usage
3. **Error rates**: Hash collisions, bloom FP rate
4. **Checkpoint time**: Serialization overhead

## Conclusion

This roadmap prioritizes improvements by impact vs. effort:
- **Phase 1** delivers 40-60% improvement with minimal risk
- **Phase 2** delivers 2-3x improvement with moderate effort
- **Phase 3** delivers 3-5x improvement for extreme scale needs

Start with Phase 1 quick wins to validate the approach, then proceed to Phase 2 based on profiling results. Only implement Phase 3 if scale testing demonstrates the need.

Each optimization is independent and can be cherry-picked based on actual bottlenecks observed in production workloads.
