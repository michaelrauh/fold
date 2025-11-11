# Hierarchical Processing Analysis for Fold

## Executive Summary

This document analyzes an alternative processing approach for the Fold text processing system. The current implementation uses a **linear file ingestion pattern** where files are processed sequentially, building incrementally on a single interner and result set. The proposed **hierarchical processing approach** would process text in parallel hierarchies, combining intermediate results to form final outputs.

Two architectural variants are examined:
1. **Pure Hierarchy**: Tree structure where results never merge back into lower levels
2. **Folding Results**: Results can be reincorporated at different hierarchy levels

## Current System: Linear File Ingestion

### Architecture Overview

The current Fold system operates on a sequential file processing model:

```
Input Files (sorted) → Process File 1 → Process File 2 → ... → Final Result
                           ↓                ↓
                    Update Interner    Update Interner
                    Update Results     Update Results
                    Track Seen IDs     Track Seen IDs
```

### Key Characteristics

1. **Sequential Processing**
   - Files in `fold_state/input/` are sorted alphabetically and processed one at a time
   - Each file extends the global interner vocabulary and phrase mappings
   - The interner version increments with each file
   - Results accumulate in a single global results queue

2. **State Management**
   - Single global interner that grows with each file
   - Single global seen tracker (bloom filter + disk-backed hash shards)
   - Single global results queue (disk-backed, persistent)
   - Checkpoint system saves state between files

3. **Memory Model**
   - Dynamically calculated memory budget (75% of system RAM)
   - Work queue with disk overflow (BFS frontier processing)
   - Results queue with disk overflow (accumulates all generated orthos)
   - Seen tracker with disk-backed shards (deduplication)

4. **Interner Change Detection**
   - When interner vocabulary/phrases change, impacted keys are identified
   - Existing results referencing impacted keys are requeued for reprocessing
   - This ensures correctness when new text adds new vocabulary or phrase completions

### Strengths

1. **Correctness**: Global deduplication ensures no ortho is processed twice
2. **Incremental Learning**: Each file contributes to growing vocabulary
3. **Persistence**: Checkpoint system allows resumption after interruption
4. **Memory Efficiency**: Disk-backed structures prevent OOM
5. **Simplicity**: Single work queue, single results queue, linear control flow

### Limitations

1. **Sequential Bottleneck**: Cannot process multiple files in parallel
2. **Interner Coupling**: All processing depends on a single, growing interner
3. **Backtracking Overhead**: Interner changes trigger result queue scans and reprocessing
4. **Checkpoint Monolithic**: Must save entire state (interner + results + seen IDs)
5. **Memory Growth**: Seen tracker grows unbounded with result count
6. **No Incremental Results**: Must process all files to get final answer

## Proposed System: Hierarchical Processing

### Core Concept

Process text through a hierarchical tree structure where:
- Leaf nodes process raw text to produce local results
- Internal nodes combine child results to produce aggregate results
- Root node produces the final optimal ortho

```
                    Root (Final Result)
                   /                   \
              Internal 1            Internal 2
             /         \            /         \
         Leaf 1    Leaf 2      Leaf 3     Leaf 4
        (Text A)  (Text B)    (Text C)   (Text D)
```

### Two Architectural Variants

#### Variant A: Pure Hierarchy

Results flow upward only, never returning to lower levels.

**Structure:**
- Each node has its own interner and result set
- Leaf nodes process text to create orthos
- Internal nodes merge child orthos and interners
- No backpropagation of results

**Pseudocode:**
```
function process_leaf(text):
    interner = Interner::from_text(text)
    results = generate_orthos(interner, seed_ortho)
    return (interner, results, optimal_ortho)

function process_internal(children):
    merged_interner = merge_interners([c.interner for c in children])
    merged_results = deduplicate([c.results for c in children])
    
    # Process merged results with merged interner
    work_queue = merged_results
    while work_queue not empty:
        ortho = work_queue.pop()
        completions = merged_interner.intersect(ortho.requirements())
        for child_ortho in expand(ortho, completions):
            if not seen(child_ortho):
                work_queue.push(child_ortho)
                merged_results.add(child_ortho)
    
    return (merged_interner, merged_results, find_optimal(merged_results))

function process_root(tree):
    if tree is leaf:
        return process_leaf(tree.text)
    else:
        children = [process_root(child) for child in tree.children]
        return process_internal(children)
```

#### Variant B: Folding Results

Results can be reincorporated at any level, creating feedback loops.

**Structure:**
- Results from higher levels can inform lower level processing
- Leaf nodes can receive "hints" from parent processing
- Interners can be shared or specialized per subtree
- Bidirectional result flow

**Pseudocode:**
```
function process_with_folding(tree, parent_hints=None):
    if tree is leaf:
        interner = Interner::from_text(tree.text)
        seed = seed_ortho()
        
        # Incorporate parent hints if available
        if parent_hints:
            seed = merge(seed, select_relevant(parent_hints, interner))
        
        results = generate_orthos(interner, seed)
        return (interner, results, optimal_ortho)
    
    else:
        # First pass: process children
        children_results = [process_with_folding(child) for child in tree.children]
        
        # Merge interners and results
        merged_interner = merge_interners([c.interner for c in children_results])
        merged_results = deduplicate([c.results for c in children_results])
        
        # Generate new orthos at this level
        new_results = generate_orthos(merged_interner, merged_results)
        
        # Second pass: fold back promising results to children
        if should_fold_back(new_results):
            hints = select_promising(new_results)
            refined_children = [
                process_with_folding(child, hints) 
                for child in tree.children
            ]
            return process_internal(refined_children)
        
        return (merged_interner, new_results, find_optimal(new_results))
```

## Necessary Implementation Steps

### Phase 1: Core Infrastructure

#### 1.1 Tree Data Structure
```rust
pub struct ProcessingNode {
    id: usize,
    children: Vec<ProcessingNode>,
    text: Option<String>,  // Only for leaf nodes
    interner: Option<Interner>,
    results: Option<DiskBackedQueue<Ortho>>,
    optimal: Option<Ortho>,
}
```

#### 1.2 Interner Merging
```rust
impl Interner {
    pub fn merge(interners: &[&Interner]) -> Self {
        // Combine vocabularies
        let mut merged_vocab = Vec::new();
        let mut version = 1;
        
        for interner in interners {
            for word in &interner.vocabulary {
                if !merged_vocab.contains(word) {
                    merged_vocab.push(word.clone());
                }
            }
            version = version.max(interner.version);
        }
        
        // Rebuild prefix_to_completions with merged vocabulary
        let mut all_phrases = Vec::new();
        // ... collect phrases from all interners ...
        
        Interner {
            version: version + 1,
            vocabulary: merged_vocab,
            prefix_to_completions: Self::build_prefix_to_completions(
                &all_phrases, &merged_vocab, merged_vocab.len(), None
            ),
        }
    }
}
```

#### 1.3 Result Set Merging
```rust
pub struct ResultMerger {
    seen_tracker: SeenTracker,
    merged_results: DiskBackedQueue<Ortho>,
}

impl ResultMerger {
    pub fn merge(result_sets: Vec<DiskBackedQueue<Ortho>>) -> Self {
        let mut merger = ResultMerger::new();
        
        for mut result_set in result_sets {
            while let Some(ortho) = result_set.pop()? {
                let id = ortho.id();
                if !merger.seen_tracker.contains(&id) {
                    merger.seen_tracker.insert(id);
                    merger.merged_results.push(ortho)?;
                }
            }
        }
        
        merger
    }
}
```

### Phase 2: Hierarchical Processing Engine

#### 2.1 Parallel Processing (Pure Hierarchy)
```rust
pub struct HierarchicalProcessor {
    memory_config: MemoryConfig,
}

impl HierarchicalProcessor {
    pub fn process_tree(&self, tree: &ProcessingNode) -> Result<ProcessingResult, FoldError> {
        if tree.is_leaf() {
            self.process_leaf(tree)
        } else {
            // Process children (potentially in parallel)
            let child_results: Vec<ProcessingResult> = tree.children
                .iter()
                .map(|child| self.process_tree(child))
                .collect::<Result<Vec<_>, _>>()?;
            
            self.merge_and_process(child_results)
        }
    }
    
    fn process_leaf(&self, node: &ProcessingNode) -> Result<ProcessingResult, FoldError> {
        let text = node.text.as_ref().unwrap();
        let interner = Interner::from_text(text);
        
        // Standard BFS processing
        let mut work_queue = DiskBackedQueue::new(self.memory_config.queue_buffer_size)?;
        let mut results = DiskBackedQueue::new(self.memory_config.queue_buffer_size)?;
        let mut tracker = SeenTracker::new(/* ... */);
        
        let seed = Ortho::new(interner.version());
        work_queue.push(seed)?;
        
        // ... BFS loop as in current main.rs ...
        
        Ok(ProcessingResult {
            interner,
            results,
            optimal: find_optimal(&results),
        })
    }
    
    fn merge_and_process(&self, children: Vec<ProcessingResult>) 
        -> Result<ProcessingResult, FoldError> {
        
        // Merge interners
        let interners: Vec<&Interner> = children.iter()
            .map(|c| &c.interner)
            .collect();
        let merged_interner = Interner::merge(&interners);
        
        // Merge and deduplicate results
        let result_sets = children.into_iter().map(|c| c.results).collect();
        let mut merger = ResultMerger::merge(result_sets)?;
        
        // Process merged results with merged interner
        let mut work_queue = DiskBackedQueue::new(self.memory_config.queue_buffer_size)?;
        
        // Re-seed work queue with merged results
        while let Some(ortho) = merger.merged_results.pop()? {
            work_queue.push(ortho)?;
        }
        
        // Continue BFS with merged interner
        // ... similar to leaf processing but with merged state ...
        
        Ok(ProcessingResult {
            interner: merged_interner,
            results: /* ... */,
            optimal: /* ... */,
        })
    }
}
```

#### 2.2 Folding Variant
```rust
impl HierarchicalProcessor {
    pub fn process_with_folding(&self, tree: &ProcessingNode, hints: Option<&[Ortho]>) 
        -> Result<ProcessingResult, FoldError> {
        
        if tree.is_leaf() {
            let mut result = self.process_leaf(tree)?;
            
            // Incorporate hints from parent
            if let Some(hint_orthos) = hints {
                for hint in hint_orthos {
                    if is_relevant(hint, &result.interner) {
                        result.work_queue.push(hint.clone())?;
                    }
                }
                // Re-process with hints
                result = self.continue_processing(result)?;
            }
            
            Ok(result)
        } else {
            // First pass: process children
            let child_results: Vec<ProcessingResult> = tree.children
                .iter()
                .map(|child| self.process_with_folding(child, None))
                .collect::<Result<Vec<_>, _>>()?;
            
            let mut merged = self.merge_and_process(child_results)?;
            
            // Determine if folding would be beneficial
            if should_fold(&merged) {
                let hints = select_promising_orthos(&merged.results, 100)?;
                
                // Second pass: reprocess children with hints
                let refined_children: Vec<ProcessingResult> = tree.children
                    .iter()
                    .map(|child| self.process_with_folding(child, Some(&hints)))
                    .collect::<Result<Vec<_>, _>>()?;
                
                merged = self.merge_and_process(refined_children)?;
            }
            
            Ok(merged)
        }
    }
}
```

### Phase 3: Input File Organization

#### 3.1 Tree Builder
```rust
pub struct TreeBuilder {
    branching_factor: usize,  // e.g., 4 (quaternary tree)
}

impl TreeBuilder {
    pub fn build_from_files(&self, input_dir: &str) -> Result<ProcessingNode, FoldError> {
        let mut files = get_input_files(input_dir)?;
        files.sort();
        
        if files.is_empty() {
            return Err(FoldError::NoInput);
        }
        
        self.build_tree_recursive(files)
    }
    
    fn build_tree_recursive(&self, files: Vec<String>) -> Result<ProcessingNode, FoldError> {
        if files.len() == 1 {
            // Leaf node
            let text = fs::read_to_string(&files[0])?;
            Ok(ProcessingNode {
                id: hash(&files[0]),
                children: vec![],
                text: Some(text),
                interner: None,
                results: None,
                optimal: None,
            })
        } else {
            // Internal node
            let chunk_size = (files.len() + self.branching_factor - 1) / self.branching_factor;
            let children: Vec<ProcessingNode> = files
                .chunks(chunk_size)
                .map(|chunk| self.build_tree_recursive(chunk.to_vec()))
                .collect::<Result<Vec<_>, _>>()?;
            
            Ok(ProcessingNode {
                id: hash_children(&children),
                children,
                text: None,
                interner: None,
                results: None,
                optimal: None,
            })
        }
    }
}
```

#### 3.2 Modified Main Entry Point
```rust
fn main() -> Result<(), FoldError> {
    let input_dir = "./fold_state/input";
    
    println!("[fold] Starting hierarchical processing");
    
    let tree_builder = TreeBuilder::new(4);  // branching factor = 4
    let tree = tree_builder.build_from_files(input_dir)?;
    
    let memory_config = MemoryConfig::calculate(0, 0);
    let processor = HierarchicalProcessor::new(memory_config);
    
    // Choose processing mode
    let result = if use_folding {
        processor.process_with_folding(&tree, None)?
    } else {
        processor.process_tree(&tree)?
    };
    
    println!("\n[fold] ===== FINAL OPTIMAL ORTHO =====");
    print_optimal(&result.optimal, &result.interner);
    
    Ok(())
}
```

### Phase 4: Checkpoint and Recovery

#### 4.1 Hierarchical Checkpointing
```rust
pub struct HierarchicalCheckpoint {
    tree_structure: ProcessingNode,
    completed_nodes: HashSet<usize>,  // Node IDs that finished processing
    node_states: HashMap<usize, NodeState>,
}

pub struct NodeState {
    interner: Option<Interner>,
    results_path: Option<String>,
    optimal: Option<Ortho>,
}

impl HierarchicalCheckpoint {
    pub fn save(&self, checkpoint_dir: &str) -> Result<(), FoldError> {
        // Save tree structure
        let tree_path = format!("{}/tree.bin", checkpoint_dir);
        let encoded = bincode::encode_to_vec(&self.tree_structure, bincode::config::standard())?;
        fs::write(tree_path, encoded)?;
        
        // Save node states
        for (node_id, state) in &self.node_states {
            let node_dir = format!("{}/node_{}", checkpoint_dir, node_id);
            fs::create_dir_all(&node_dir)?;
            
            if let Some(ref interner) = state.interner {
                let interner_encoded = bincode::encode_to_vec(interner, bincode::config::standard())?;
                fs::write(format!("{}/interner.bin", node_dir), interner_encoded)?;
            }
            
            // Results already on disk, just record path
            // Optimal ortho saved separately
        }
        
        Ok(())
    }
    
    pub fn load(checkpoint_dir: &str) -> Result<Option<Self>, FoldError> {
        // Check if checkpoint exists
        let tree_path = format!("{}/tree.bin", checkpoint_dir);
        if !Path::new(&tree_path).exists() {
            return Ok(None);
        }
        
        // Load tree and node states
        // Resume from where we left off
        // ...
        
        Ok(Some(checkpoint))
    }
}
```

## Benefits of Hierarchical Processing

### Performance Benefits

1. **Parallelization Potential**
   - Leaf nodes are completely independent and can be processed in parallel
   - Internal nodes can process children in parallel (within resource constraints)
   - Could utilize multi-core CPUs effectively
   - Potential for distributed processing across machines

2. **Incremental Results**
   - Partial results available as subtrees complete
   - Can identify optimal orthos in subtrees before full completion
   - Enables early termination if optimal is "good enough"

3. **Reduced Interner Backtracking**
   - Pure hierarchy: No interner changes propagate downward
   - Each subtree has stable interner during its processing
   - No need to rescan entire result set when vocabulary changes

4. **Memory Locality**
   - Each subtree processes its own working set
   - Better cache utilization per subtree
   - Can process large datasets that don't fit in single-machine memory

5. **Divide and Conquer Efficiency**
   - Complexity reduced through decomposition
   - Each level processes smaller problem space
   - Merge operations are parallelizable

### Operational Benefits

1. **Fault Tolerance**
   - Failure in one subtree doesn't lose all work
   - Can checkpoint per-node instead of monolithic checkpoint
   - Easier to retry failed subtrees

2. **Resource Flexibility**
   - Can allocate different resources to different subtrees
   - Large subtrees get more memory/disk
   - Small subtrees process quickly with minimal resources

3. **Monitoring and Debugging**
   - Progress tracked per subtree
   - Can identify slow subtrees and investigate
   - Easier to reason about memory usage per subtree

4. **Scalability**
   - Adding more files doesn't linearly increase interner size
   - Each subtree has bounded interner growth
   - Root merging is bounded by depth, not file count

## Drawbacks of Hierarchical Processing

### Correctness Concerns

1. **Duplicate Detection Complexity**
   - Must track seen IDs across entire tree
   - Merging seen trackers is complex (bloom filter union)
   - Risk of duplicates if merging is incorrect
   - Higher memory for per-node trackers vs global tracker

2. **Interner Semantics**
   - Merging interners loses some phrase context
   - Phrases valid in one subtree may not be valid in merged interner
   - Version numbering becomes complex (per-node vs global)
   - Completions may differ between subtree interner and merged interner

3. **Optimal Finding**
   - Optimal in subtree may not be optimal globally
   - Must revalidate subtree optimals with merged interner
   - Global optimal might require completions from merged vocabulary
   - No guarantee hierarchical processing finds same optimal as linear

### Implementation Complexity

1. **Significant Code Changes**
   - Current system is ~350 lines of main.rs
   - Hierarchical system would be 1000+ lines
   - New data structures (tree, merger, hierarchical checkpoint)
   - More complex state management

2. **Interner Merge Logic**
   - Complex algorithm to merge vocabularies and prefix mappings
   - Must preserve closure property (all prefixes have completions)
   - Must handle version conflicts
   - Must rebuild prefix_to_completions for new vocabulary size

3. **Testing Challenges**
   - Many more edge cases (tree depth, branching factor, merge scenarios)
   - Harder to reproduce bugs (depends on tree structure)
   - Need to test equivalence with linear version
   - Performance testing requires realistic large datasets

4. **Debugging Difficulty**
   - Harder to trace execution flow through tree
   - Parallel execution makes debugging non-deterministic
   - Multiple interners and result sets to inspect
   - Merge bugs can be subtle

### Performance Concerns

1. **Merge Overhead**
   - Interner merging is O(V) where V is total vocabulary
   - Result set deduplication requires hashing every ortho
   - Bloom filter operations per merge
   - Disk I/O for reading/writing multiple result sets

2. **Memory Pressure**
   - Each subtree needs its own work queue, results queue, tracker
   - Peak memory when merging multiple large result sets
   - More total bloom filters (one per node) vs single global one
   - Temporary buffers for merging

3. **Disk I/O Amplification**
   - Results written to disk at each tree level
   - Re-reading results during merge
   - More checkpoint data (per-node state)
   - Potential disk space exhaustion with deep trees

4. **Parallelization Overhead**
   - Thread/process spawning costs
   - Synchronization overhead
   - Resource contention (disk, RAM)
   - Diminishing returns with too many parallel tasks

### Operational Drawbacks

1. **Complexity of Setup**
   - User must understand tree structure implications
   - Need to choose branching factor
   - File organization affects tree shape
   - Less intuitive than linear file processing

2. **Checkpoint Size**
   - More checkpoint data (per-node state)
   - Harder to inspect checkpoint contents
   - Recovery more complex

3. **Monitoring Difficulty**
   - Progress is multi-dimensional (per-node)
   - Hard to estimate completion time
   - Resource usage varies by tree level

### Folding Variant Specific Issues

1. **Convergence Uncertainty**
   - How many fold iterations are needed?
   - Diminishing returns after first fold?
   - Risk of infinite loops if not carefully designed

2. **Hint Selection Complexity**
   - Which results should be folded back?
   - How to filter relevant hints for each subtree?
   - Overhead of multiple processing passes

3. **Increased Runtime**
   - Folding requires multiple passes over children
   - More total work than pure hierarchy
   - May not improve result quality enough to justify cost

## Final State Comparison

### Linear (Current State)

**After Processing All Files:**
```
Global State:
├── Interner (version N, vocabulary size V)
├── Results Queue (R orthos on disk)
├── Seen Tracker (R IDs in bloom + shards)
└── Optimal Ortho (global best)

Disk Layout:
fold_state/
├── input/ (empty - files deleted after processing)
├── results/ (queue_*.bin files)
├── checkpoint/
│   ├── interner.bin
│   └── results_backup/ (queue_*.bin files)
└── seen_tracker/ (shard_*.bin files)
```

**Characteristics:**
- Single interner with all vocabulary
- All results in one deduplicated set
- One optimal ortho (highest score globally)
- Deterministic results (same order → same optimal)

### Pure Hierarchy (Final State)

**After Processing Complete Tree:**
```
Tree State (4-ary tree, 16 leaf files):
                     Root
                   /  |  \  \
            N1    N2   N3   N4 (internal nodes)
           /|\|  /|\|  /|\|  /|\|
         L1..L4 L5..L8 L9..L12 L13..L16 (leaves)

Each Node Contains:
- Interner (merged from children or built from text)
- Results Queue (merged + expanded from children)
- Optimal Ortho (best in subtree)

Root Contains:
- Final merged interner
- Final merged results
- Global optimal ortho
```

**Disk Layout:**
```
fold_state/
├── input/ (empty - files deleted)
├── tree_checkpoint/
│   ├── tree.bin (structure)
│   ├── node_ROOT/
│   │   ├── interner.bin
│   │   ├── results/ (queue_*.bin)
│   │   └── optimal.bin
│   ├── node_N1/
│   │   ├── interner.bin
│   │   ├── results/ (queue_*.bin)
│   │   └── optimal.bin
│   └── ... (for each node)
└── temp_merge_buffers/ (cleaned up after completion)
```

**Characteristics:**
- Multiple interners (one per node, eventually merged)
- Results distributed across tree, merged upward
- Optimal per subtree, final optimal at root
- May differ from linear results (due to different merge order)

### Folding Variant (Final State)

**After Folding Iterations:**
```
Tree State (after 2 fold iterations):
                     Root (refined)
                   /  |  \  \
            N1'   N2'  N3'  N4' (refined internal nodes)
           /|\|  /|\|  /|\|  /|\|
         L1'..L4' L5'..L8' L9'..L12' L13'..L16' (refined leaves)

Refinement means:
- Leaves reprocessed with hints from parents
- Internal nodes remerged with refined children
- Multiple passes until convergence

Each Node Contains:
- Interner (stable after first pass)
- Results Queue (augmented with hints)
- Optimal Ortho (refined through feedback)
```

**Disk Layout:**
```
fold_state/
├── input/ (empty)
├── tree_checkpoint/
│   ├── tree.bin
│   ├── fold_iteration_1/ (first pass)
│   │   └── ... (node states)
│   ├── fold_iteration_2/ (second pass)
│   │   └── ... (refined node states)
│   └── fold_final/ (final state)
│       └── ... (converged node states)
└── hints_cache/ (promising orthos to fold back)
```

**Characteristics:**
- Iterative refinement of results
- More disk space (multiple iterations saved)
- Potentially better optimal (from folding)
- Non-deterministic (depends on hint selection)

## Technical Considerations

### Memory Management

**Pure Hierarchy:**
- Memory per node = interner + work_queue + results_queue + tracker
- Peak memory = max(simultaneous nodes) × per_node_memory
- Can control via tree depth and parallel execution limit
- Example: 4 parallel leaves × 1GB each = 4GB peak

**Folding:**
- Additional memory for hints cache
- Multiple iterations mean more temporary state
- Peak memory during merge + fold hint selection

### Disk Usage

**Linear:**
- Work queue: transient (cleared between files)
- Results queue: grows throughout (~200-400 bytes × R orthos)
- Checkpoint: interner (~50-500MB) + results backup
- Total: ~R × 400 bytes + 500MB

**Hierarchical:**
- Per-node results: R_total/N nodes × 400 bytes (distributed)
- Per-node interners: smaller than linear (V_subtree < V_total)
- Merge buffers: transient, up to 2× result size during merge
- Total: potentially 2-3× linear due to per-node overhead

### Correctness Guarantees

**Linear:**
- ✅ No duplicate orthos (global seen tracker)
- ✅ All valid expansions explored (BFS exhaustive)
- ✅ Deterministic results (fixed file order)
- ✅ Correct interner (incrementally built)

**Hierarchical Pure:**
- ✅ No duplicates within subtree
- ⚠️  Duplicates possible across subtrees if merge buggy
- ✅ All valid expansions within subtree
- ⚠️  May miss expansions requiring cross-subtree phrases
- ❌ Non-deterministic (depends on tree structure)
- ⚠️  Merged interner may have different semantics

**Hierarchical Folding:**
- ⚠️  Duplicate risk higher (multiple iterations)
- ⚠️  Convergence not guaranteed
- ⚠️  Hint selection may bias exploration
- ❌ Highly non-deterministic

## Recommendation

### When to Use Linear (Current System)

**Best for:**
- Small to medium datasets (< 1M orthos)
- When deterministic results are required
- When correctness is paramount
- When simplicity is valued
- When disk space is limited
- When debugging is frequent

**Example scenarios:**
- Processing a single book (10-20 files)
- Research/development work
- Validation of algorithm correctness
- Small production workloads

### When to Consider Pure Hierarchy

**Best for:**
- Large datasets (> 10M orthos)
- When processing time is critical
- When partial results are useful
- When parallelization is needed
- When RAM is limited per machine but many machines available
- When subtrees are logically independent

**Example scenarios:**
- Processing massive corpora (thousands of files)
- Distributed processing across cluster
- Real-time incremental processing (new files added to tree)
- Fault-tolerant long-running jobs

**Implementation recommendation:**
- Start with proof-of-concept for small tree (depth 2, 4 leaves)
- Validate correctness against linear version
- Benchmark merge overhead
- Only proceed if 2-5× speedup is achieved

### When to Avoid Folding Variant

**Avoid if:**
- Correctness is critical
- Disk space is limited
- Implementation complexity is a concern
- Convergence behavior is unpredictable

**Consider only if:**
- Research project exploring iterative refinement
- Have proven that folding significantly improves results
- Acceptable to have non-reproducible results
- Willing to invest in complex implementation and debugging

## Conclusion

The hierarchical processing approach offers significant potential for parallelization and scalability but comes with substantial implementation complexity and correctness risks. The current linear system is correct, simple, and efficient for most workloads.

**Recommended Path Forward:**

1. **Quantify the problem first**: Measure actual processing time and bottlenecks with current linear system on target workloads.

2. **If parallelization is needed**: Implement pure hierarchy variant with careful attention to:
   - Interner merge correctness (preserve phrase closure)
   - Result deduplication (global seen tracker spanning all nodes)
   - Checkpoint per-node state
   - Validation against linear version

3. **Avoid folding variant** unless research goals require exploring iterative refinement.

4. **Incremental approach**:
   - Phase 1: Implement tree builder (organize files into tree, but still process linearly)
   - Phase 2: Implement interner merge (validate correctness with tests)
   - Phase 3: Implement parallel leaf processing (validate no duplicates)
   - Phase 4: Implement merge and process internal nodes
   - Phase 5: Validate final results match linear version
   - Phase 6: Benchmark and tune

The hierarchical approach is a significant architectural change that should only be undertaken with clear performance goals and a plan for validating correctness. The current linear system's simplicity and correctness guarantees are valuable and should not be abandoned lightly.
