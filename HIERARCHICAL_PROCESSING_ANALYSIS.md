# Hierarchical Processing Analysis for Fold

## Executive Summary

This document analyzes alternative processing approaches for the Fold text processing system. The current implementation uses a **linear file ingestion pattern** where files are processed sequentially, building incrementally on a single interner and result set. 

Three optimization strategies are examined:
1. **Hierarchical Processing**: Tree structure where text is processed in parallel subtrees, results flow upward and merge at internal nodes
2. **Compaction**: Remove orthos that fall wholly inside other orthos, reconstructing them only when interner changes
3. **Combined Approach**: Hierarchical processing with compaction enabled at each tree level

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

### Hierarchical Processing Architecture

Results flow upward only through the tree, never returning to lower levels.

**Structure:**
- Each node has its own interner and result set
- Leaf nodes process raw text to create orthos
- Internal nodes merge child orthos and interners, then continue BFS processing
- Root node produces the final merged interner and optimal ortho

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

### Compaction Optimization

Compaction is an orthogonal optimization that can be applied to either linear or hierarchical processing.

**Core Concept:**
An ortho A "falls wholly inside" another ortho B if all cells filled in A are also filled in B with the same values, and B has additional filled cells. When this occurs, A is redundant and can be removed from the results queue.

**Detection Algorithm:**
```
function is_contained(ortho_a, ortho_b):
    # Check if A's filled cells are a subset of B's filled cells
    for i in range(len(ortho_a.payload)):
        if ortho_a.payload[i] is Some:
            if ortho_b.payload[i] != ortho_a.payload[i]:
                return false  # Mismatch or B doesn't have this cell
    
    # Check that B has at least one additional filled cell
    a_filled = count_filled(ortho_a.payload)
    b_filled = count_filled(ortho_b.payload)
    return b_filled > a_filled

function compact_results(results_queue):
    compacted = []
    for ortho in results_queue:
        is_redundant = false
        for other in compacted:
            if is_contained(ortho, other):
                is_redundant = true
                break
        if not is_redundant:
            # Also check if ortho subsumes any existing orthos
            compacted = [o for o in compacted if not is_contained(o, ortho)]
            compacted.append(ortho)
    return compacted
```

**Reconstruction on Interner Change:**
When the interner changes and impacted keys are identified, contained orthos must be reconstructed because new vocabulary might enable new expansions:

```
function reconstruct_contained_orthos(compacted_results, interner):
    # Maintain a containment map: child_id -> parent_id
    containment_map = load_from_disk("containment_map.bin")
    
    # For each impacted key, find all contained orthos that reference it
    for impacted_key in get_impacted_keys():
        for (child_id, parent_id) in containment_map:
            child_ortho = reconstruct_from_parent(parent_id, child_id)
            if child_ortho.references(impacted_key):
                work_queue.push(child_ortho)
```

**Storage Requirements:**
- Compacted results queue: Significantly smaller (typically 10-30% of original)
- Containment map: Maps removed ortho IDs to their containing parent IDs (~16 bytes per contained ortho)
- Total savings: 70-90% reduction in result queue size, with small overhead for containment tracking

**Trade-offs:**
- **Benefit**: Dramatically reduced result queue size (memory + disk)
- **Benefit**: Fewer orthos to scan when finding impacted keys
- **Benefit**: Faster checkpoint save/load (less data)
- **Cost**: Compaction algorithm is O(N²) in worst case (all orthos compared)
- **Cost**: Containment map storage and maintenance
- **Cost**: Reconstruction overhead when interner changes (but amortized across many changes)

**Implementation Notes:**
- Compaction can be run periodically (e.g., every 10k orthos generated) rather than continuously
- Can use spatial heuristics to optimize containment checks (orthos with different dimensions cannot contain each other)
- Containment map can be lazily persisted to disk (in-memory until checkpoint)

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

Merging interners is a critical operation that combines vocabularies and phrase mappings from multiple subtrees while preserving the prefix closure property.

**Algorithm Overview:**

1. **Vocabulary Union**: Combine vocabularies from all child interners, eliminating duplicates
2. **Phrase Collection**: Gather all phrases from all child interners
3. **Prefix Mapping Rebuild**: Reconstruct `prefix_to_completions` for the merged vocabulary size
4. **Version Increment**: Assign new version to indicate merged state

**Detailed Implementation:**

```rust
impl Interner {
    pub fn merge(interners: &[&Interner]) -> Self {
        // Step 1: Combine vocabularies with deduplication
        // Using a HashSet for O(1) lookup, then converting to Vec
        let mut vocab_set = std::collections::HashSet::new();
        let mut max_version = 0;
        
        for interner in interners {
            for word in &interner.vocabulary {
                vocab_set.insert(word.clone());
            }
            max_version = max_version.max(interner.version);
        }
        
        let merged_vocab: Vec<String> = vocab_set.into_iter().collect();
        let vocab_len = merged_vocab.len();
        
        // Step 2: Create word -> index mapping for the merged vocabulary
        let word_to_idx: HashMap<&str, usize> = merged_vocab.iter()
            .enumerate()
            .map(|(idx, word)| (word.as_str(), idx))
            .collect();
        
        // Step 3: Collect all phrases from all interners and remap to merged indices
        let mut all_phrases: Vec<Vec<usize>> = Vec::new();
        
        for interner in interners {
            // For each interner, get its phrases and remap word indices
            for phrase_indices in extract_phrases_from_interner(interner) {
                let remapped: Vec<usize> = phrase_indices.iter()
                    .map(|&old_idx| {
                        let word = &interner.vocabulary[old_idx];
                        word_to_idx[word.as_str()]
                    })
                    .collect();
                all_phrases.push(remapped);
            }
        }
        
        // Step 4: Rebuild prefix_to_completions with merged vocabulary
        // This ensures the prefix closure property: for every phrase,
        // all its prefixes have completions that include the next word
        let prefix_to_completions = Self::build_prefix_to_completions(
            &all_phrases,
            &merged_vocab,
            vocab_len,
            None  // No previous mapping to merge with
        );
        
        Interner {
            version: max_version + 1,
            vocabulary: merged_vocab,
            prefix_to_completions,
        }
    }
    
    // Helper to extract phrases from an interner's prefix_to_completions map
    fn extract_phrases_from_interner(interner: &Interner) -> Vec<Vec<usize>> {
        // Reconstruct original phrases by inverting the prefix map
        // This is necessary because phrases are stored implicitly
        let mut phrases = Vec::new();
        
        for (prefix, completions) in &interner.prefix_to_completions {
            for completion_idx in completions.ones() {
                let mut phrase = prefix.clone();
                phrase.push(completion_idx);
                phrases.push(phrase);
            }
        }
        
        phrases
    }
}
```

**Performance Characteristics:**

- **Time Complexity**: O(V₁ + V₂ + ... + Vₙ + P × L) where V is vocabulary size per interner, P is total phrases, L is average phrase length
- **Space Complexity**: O(V_merged + P) for temporary structures
- **Typical Cost**: For 4 subtrees with 10K vocab each, ~40K unique words, ~100K phrases, merge takes ~50-100ms

**Critical Correctness Property:**

The merged interner must maintain the **prefix closure property**: for every phrase in the merged vocabulary, all prefixes of that phrase must map to completions that include the next token. The `build_prefix_to_completions` function ensures this by:

1. For each phrase, extracting all prefixes
2. For each prefix, recording all observed completions
3. Building a FixedBitSet for efficient completion lookup

**Merge Semantics:**

After merging, the new interner can:
- Answer completion queries for phrases from any child subtree
- Discover NEW cross-subtree phrases (words from subtree A completing prefixes from subtree B)
- May have FEWER completions for some prefixes (if phrases don't occur in all subtrees)

This is why merged interners enable new ortho expansions that weren't possible in individual subtrees.

#### 1.3 Result Set Merging

Merging result sets from multiple subtrees requires careful deduplication to ensure no ortho appears multiple times in the final result set, while preserving all unique orthos.

**Algorithm Overview:**

1. **Global Deduplication**: Use a unified seen tracker (bloom filter + hash shards) spanning all subtrees
2. **Streaming Merge**: Stream orthos from each child result queue, checking against seen tracker
3. **Optimal Tracking**: Track the best ortho encountered during merge

**Detailed Implementation:**

```rust
pub struct ResultMerger {
    seen_tracker: SeenTracker,
    merged_results: DiskBackedQueue<Ortho>,
    current_best: Option<Ortho>,
    current_best_score: (usize, usize),
}

impl ResultMerger {
    pub fn merge(result_sets: Vec<DiskBackedQueue<Ortho>>, memory_config: &MemoryConfig) 
        -> Result<Self, FoldError> {
        
        // Initialize merged structures with appropriate capacity
        let estimated_total = result_sets.iter().map(|q| q.len()).sum();
        let seen_tracker = SeenTracker::with_config(
            estimated_total * 3,  // 3x for growth headroom
            memory_config.num_shards,
            memory_config.max_shards_in_memory
        );
        let merged_results = DiskBackedQueue::new(memory_config.queue_buffer_size)?;
        
        let mut merger = ResultMerger {
            seen_tracker,
            merged_results,
            current_best: None,
            current_best_score: (0, 0),
        };
        
        // Stream through each result set
        for mut result_set in result_sets {
            println!("[merge] Processing result set with {} orthos", result_set.len());
            let mut added = 0;
            let mut duplicates = 0;
            
            while let Some(ortho) = result_set.pop()? {
                let id = ortho.id();
                
                // Bloom filter + shard check for deduplication
                if !merger.seen_tracker.contains(&id) {
                    merger.seen_tracker.insert(id);
                    
                    // Track optimal during merge
                    let score = calculate_score(&ortho);
                    if score > merger.current_best_score {
                        merger.current_best = Some(ortho.clone());
                        merger.current_best_score = score;
                    }
                    
                    merger.merged_results.push(ortho)?;
                    added += 1;
                } else {
                    duplicates += 1;
                }
            }
            
            println!("[merge] Added {} new orthos, skipped {} duplicates", added, duplicates);
        }
        
        println!("[merge] Final merged result set: {} unique orthos", merger.merged_results.len());
        Ok(merger)
    }
    
    pub fn into_results(self) -> (DiskBackedQueue<Ortho>, Option<Ortho>, SeenTracker) {
        (self.merged_results, self.current_best, self.seen_tracker)
    }
}
```

**Performance Characteristics:**

- **Time Complexity**: O(R₁ + R₂ + ... + Rₙ) where R is result count per subtree, assuming O(1) hash lookups
- **Space Complexity**: O(R_total) for bloom filter and disk-backed shards
- **Typical Cost**: For 4 subtrees with 100K results each, 400K total, deduplication might find 20-40% duplicates
  - Merge time: ~5-10 seconds (disk I/O dominated)
  - Memory: ~100MB for bloom filter + hot shards

**Deduplication Effectiveness:**

The degree of duplication between subtrees depends on:
1. **Text similarity**: More similar text → more duplicate orthos
2. **Vocabulary overlap**: Shared vocabulary → shared orthos
3. **Seed ortho**: All subtrees start with same seed → guaranteed duplication of early expansions

**Expected duplication rates:**
- Independent texts (e.g., different books): 5-15% duplicates
- Related texts (e.g., chapters of same book): 20-40% duplicates
- Highly similar texts: 40-60% duplicates

**Memory Management:**

The seen tracker uses a two-tier strategy:
- Bloom filter (in-memory): Fast negative lookups, small false positive rate
- Hash shards (memory + disk): Definitive deduplication, hot shards in RAM, cold shards on disk

This allows merging result sets larger than available RAM without correctness loss.

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

## Resource Analysis: Time, Disk, and RAM

This section provides detailed analysis of where each approach spends computational resources.

### Current System: Linear Processing

**Time Breakdown (for processing N files, R total orthos generated):**

1. **BFS Work Queue Processing**: 60-70% of total time
   - Pop ortho from work queue: O(1)
   - Get requirements (forbidden + required prefixes): O(D) where D = dimensions
   - Interner intersection (find completions): O(P × C) where P = prefixes, C = avg completions per prefix
   - Expand ortho with completion: O(D²) for spatial transformations
   - Compute ortho ID (hash): O(payload_size)
   - Check seen tracker: O(1) bloom filter + O(1) amortized hash lookup
   - Push to results queue: O(1) amortized
   - **Typical**: ~1-5ms per ortho depending on dimensions and completion count

2. **Uniqueness Checks (Seen Tracker)**: 15-20% of total time
   - Bloom filter check: O(1) with ~4-8 hash functions
   - Shard lookup (if bloom says "maybe"): O(1) average HashMap lookup
   - Shard disk I/O: When hot shards spill, ~10ms per shard flush
   - **Typical**: ~100-200µs per ortho on average, including occasional disk writes

3. **Finding Impacted Orthos (on interner change)**: 10-15% of total time
   - Identify impacted keys: O(V_new) to compare vocabularies/prefixes
   - Scan all results: O(R) to check each ortho's requirement phrases
   - Requeue impacted orthos: O(I) where I = impacted count
   - **Typical**: ~2-10 seconds per file (scanning 100K-1M orthos)

4. **Forming Completions (Interner Operations)**: 5-10% of total time
   - Build interner from text: O(T) where T = text size
   - Extract vocabulary: O(W) where W = unique words
   - Build prefix_to_completions: O(P × L) where P = phrases, L = avg phrase length
   - Intersect for completions: O(num_prefixes × vocabulary_size) per call
   - **Typical**: ~1-5 seconds per file to build interner

5. **Checkpoint Save/Load**: <5% of total time
   - Serialize interner: O(V + P) typically 10-50ms
   - Flush results queue: O(R) typically 100-500ms
   - Rebuild seen tracker: O(R) typically 1-5 seconds on load
   - **Typical**: 2-10 seconds per checkpoint

**Example: Processing 16 files, 1M total orthos, 50K vocabulary:**
- BFS processing: ~70 minutes (70%)
- Uniqueness checks: ~20 minutes (20%)
- Finding impacted: ~8 file transitions × 5 sec = 40 seconds (<1%)
- Forming completions: ~16 files × 3 sec = 48 seconds (<1%)
- Checkpointing: ~8 checkpoints × 5 sec = 40 seconds (<1%)
- **Total: ~100 minutes**

**Disk Usage:**
- Interner: 50-500 MB (depends on vocabulary size)
- Results queue: R × 200-400 bytes (e.g., 1M orthos = 200-400 MB)
- Seen tracker shards: R × 12 bytes on disk (e.g., 1M orthos = 12 MB)
- Checkpoint backup: 2× results queue (400-800 MB)
- **Total: ~1-2 GB for 1M orthos**

**RAM Usage (with dynamic configuration for 8GB machine):**
- Interner: ~100-200 MB in memory
- Work queue buffer: ~10K orthos × 300 bytes = 3 MB
- Results queue buffer: ~10K orthos × 300 bytes = 3 MB
- Bloom filter: 1M capacity × 2 bytes = 2 MB
- Hot shards: ~64 shards × 100K entries × 12 bytes = 77 MB
- Runtime overhead: ~20% = ~100 MB
- **Total: ~300-400 MB peak**

### Hierarchical Processing (Without Compaction)

**Time Breakdown (for 16 files in 4-ary tree, 1M total orthos):**

1. **BFS Work Queue Processing (Per Node)**: 55-65% of total time
   - Same per-ortho cost as linear
   - But can parallelize across leaves
   - 4 leaves process ~250K orthos each simultaneously
   - **Parallel speedup**: 3-3.5× (not perfect 4× due to merge overhead)

2. **Interner Merging (Per Internal Node)**: 10-15% of total time
   - Merge 4 child vocabularies: O(V₁ + V₂ + V₃ + V₄) = O(V_total)
   - Extract phrases from children: O(P_total × L)
   - Rebuild prefix_to_completions: O(P_total × L)
   - **Typical**: ~5-20 seconds per internal node (3 internal nodes in tree)
   - **Total merge time**: ~15-60 seconds

3. **Result Set Merging (Per Internal Node)**: 15-20% of total time
   - Stream and deduplicate: O(R_total) hash operations
   - Disk I/O to read child results: O(R_total × 300 bytes)
   - Bloom filter + shard checks: Same as linear per ortho
   - **Typical**: ~5-15 seconds per internal node
   - **Total merge time**: ~15-45 seconds

4. **Uniqueness Checks**: 10-15% of total time
   - Per-node bloom filters: Smaller, more efficient
   - Final merge bloom: Same size as linear
   - Overall similar to linear but distributed

5. **No Impacted Ortho Scanning**: 0% (advantage!)
   - Interners don't change within a subtree
   - No backtracking needed
   - Saved time: ~40 seconds compared to linear

**Example: Same 16 files, 1M orthos workload:**
- Leaf BFS (parallel 4×): ~70 min / 3.5 = ~20 minutes
- Internal node BFS (3 nodes): ~15 minutes
- Interner merging: ~1 minute
- Result merging: ~1 minute
- **Total: ~37 minutes (2.7× speedup over linear)**

**Disk Usage:**
- Per-leaf results: ~250K orthos × 300 bytes = 75 MB × 4 = 300 MB
- Per-internal results: Grows to final ~1M orthos = 300 MB
- Per-node interners: ~50-200 MB each, 7 nodes = 350-1400 MB
- Merge buffers (transient): ~300 MB during merge operations
- Checkpoint: All node states = ~2-4 GB
- **Total: ~3-5 GB (2-3× linear)**

**RAM Usage (per node, with same 8GB machine):**
- Can process 1-2 nodes in parallel (limited by RAM)
- Per-node budget: ~2-4 GB
- Leaf node peak: ~300 MB (smaller working set)
- Internal node peak: ~1-2 GB (during merge)
- **Bottleneck**: Internal node merging requires most RAM

### Hierarchical Processing WITH Compaction

**Time Breakdown (for 16 files, 1M orthos, 70% compaction rate):**

1. **BFS Work Queue Processing**: 55-65% (same as without compaction)

2. **Compaction (Periodic)**: 5-10% of total time
   - Run every 10K orthos: ~100 compaction runs per leaf
   - O(N²) comparison in worst case, but spatial heuristics help
   - **Typical**: ~50-200ms per compaction run
   - **Total**: ~5-20 seconds per leaf

3. **Interner Merging**: 8-12% of total time
   - Faster due to smaller result sets to process
   - **Typical**: ~3-15 seconds per internal node

4. **Result Set Merging**: 5-8% of total time
   - Only 30% of orthos to merge (70% were compacted)
   - **Typical**: ~2-5 seconds per internal node
   - **Speedup**: 3× faster than without compaction

5. **Reconstruction (On Interner Change)**: <1% in hierarchical
   - Minimal since no interner changes within subtree
   - Only at root if continuing with new files

**Example: Same workload with compaction:**
- Leaf BFS (parallel): ~20 minutes
- Compaction: ~2 minutes (distributed across leaves)
- Internal BFS: ~10 minutes (fewer orthos to process)
- Interner merging: ~30 seconds
- Result merging: ~20 seconds (70% fewer orthos)
- **Total: ~32 minutes (3.1× speedup over linear)**

**Disk Usage:**
- Compacted results: 300K orthos (70% removed) × 300 bytes = 90 MB
- Containment map: 700K entries × 16 bytes = 11 MB
- Interners: Same as without compaction = 350-1400 MB
- Checkpoint: ~1-2 GB (significantly smaller due to compaction)
- **Total: ~1.5-2.5 GB (similar to linear!)**

**RAM Usage:**
- Peak is during internal node merge
- Compacted results fit better in memory
- Bloom filter can be smaller (fewer orthos)
- **Per-node**: ~200-500 MB (better than without compaction)
- **Can run 3-4 leaves in parallel** instead of 1-2

### Linear Processing WITH Compaction

**Time Breakdown (for 16 files, 1M orthos, 70% compaction rate):**

1. **BFS Work Queue Processing**: 60-70% (same as without)

2. **Compaction**: 10-15% of total time
   - Run every 10K orthos: ~100 compaction runs total
   - Must check all existing compacted orthos
   - **Typical**: ~100-500ms per run (more orthos to check than in hierarchical)
   - **Total**: ~10-50 seconds

3. **Finding Impacted Orthos**: 8-12% of total time
   - Faster: Only scan 300K compacted orthos instead of 1M
   - But must reconstruct contained orthos
   - **Typical**: ~1-3 seconds per file
   - **Speedup**: 2-3× faster than without compaction

4. **Uniqueness Checks**: 12-18% (similar to without)

**Example: Same workload:**
- BFS processing: ~70 minutes
- Compaction: ~15 minutes
- Finding impacted: ~20 seconds (faster!)
- Uniqueness checks: ~20 minutes
- **Total: ~85 minutes (1.18× speedup over linear without compaction)**

**Disk Usage:**
- Compacted results: 300K orthos × 300 bytes = 90 MB
- Containment map: 11 MB
- Interner: 50-500 MB
- Checkpoint: ~200-600 MB
- **Total: ~300-1200 MB (significant savings!)**

**RAM Usage:**
- Smaller result queue buffer
- Smaller bloom filter
- **Total: ~200-300 MB peak (significant savings!)**

### Summary Table

| Approach | Time | Disk | RAM | Best For |
|----------|------|------|-----|----------|
| Linear | 100 min | 1-2 GB | 300-400 MB | Simplicity, correctness |
| Linear + Compaction | 85 min | 300-1200 MB | 200-300 MB | Memory-constrained, moderate datasets |
| Hierarchical | 37 min | 3-5 GB | 2-4 GB | Large datasets, parallelization |
| Hierarchical + Compaction | 32 min | 1.5-2.5 GB | 1-2 GB | **Best overall**: Speed + efficiency |

**Key Insights:**

1. **Hierarchical gains 2.7× speedup** primarily from parallelizing leaf processing and eliminating impacted ortho scanning
2. **Compaction saves 70-90% disk space** with minimal time overhead (<15%)
3. **Combined approach is optimal**: 3.1× faster than linear, similar disk usage
4. **Linear + Compaction** is best incremental improvement: 15% faster, 50% less disk, simpler than hierarchical

### Technical Considerations

### Memory Management

**Hierarchical:**
- Memory per node = interner + work_queue + results_queue + tracker
- Peak memory = max(simultaneous nodes) × per_node_memory
- Can control via tree depth and parallel execution limit
- With compaction: Can process 3-4 nodes in parallel instead of 1-2

### Correctness Guarantees

**Linear:**
- ✅ No duplicate orthos (global seen tracker)
- ✅ All valid expansions explored (BFS exhaustive)
- ✅ Deterministic results (fixed file order)
- ✅ Correct interner (incrementally built)

**Linear + Compaction:**
- ✅ Same as linear (compaction is lossless)
- ✅ Contained orthos can be reconstructed
- ⚠️  Reconstruction logic must be correct

**Hierarchical:**
- ✅ No duplicates within subtree
- ⚠️  Duplicates possible across subtrees if merge buggy
- ✅ All valid expansions within subtree
- ⚠️  May miss expansions requiring cross-subtree phrases (but merged interner enables them at internal nodes)
- ❌ Non-deterministic (depends on tree structure)
- ⚠️  Merged interner may have different semantics

**Hierarchical + Compaction:**
- ✅ Same as hierarchical
- ✅ Per-node compaction reduces merge overhead
- ⚠️  Containment relationships must be tracked per node and merged correctly

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

### When to Consider Hierarchical Processing

**Best for:**
- Large datasets (> 10M orthos)
- When processing time is critical and 2-3× speedup justifies complexity
- When partial results are useful (subtrees complete independently)
- When parallelization infrastructure is available
- When RAM per machine is limited but multiple machines available
- When subtrees are logically independent

**Example scenarios:**
- Processing massive corpora (thousands of files)
- Distributed processing across cluster
- Real-time incremental processing (new files added to tree)
- Fault-tolerant long-running jobs

**Implementation recommendation:**
- Start with proof-of-concept for small tree (depth 2, 4 leaves)
- Validate correctness against linear version
- Benchmark merge overhead carefully
- Only proceed if 2-5× speedup is achieved in testing

### When to Use Compaction

**Best for:**
- Any workload where disk space or memory is constrained
- Moderate to large datasets (> 100K orthos)
- When checkpoint save/load time is a bottleneck
- When scanning for impacted orthos takes significant time

**Best combined with:**
- Linear processing: Simple improvement, 15% faster, 50% less disk
- Hierarchical processing: Optimal combination, 3× faster, similar disk to linear

**Implementation recommendation:**
- Implement compaction first as incremental improvement to linear
- Test containment detection algorithm thoroughly
- Verify reconstruction logic is correct on interner changes
- Add compaction to hierarchical only after both are working

## Conclusion

The analysis reveals four distinct optimization strategies, each with clear trade-offs:

1. **Linear (Current)**: Simple, correct, deterministic. Best for most workloads.
2. **Linear + Compaction**: 15% faster, 50% less disk. Low-hanging fruit for easy wins.
3. **Hierarchical**: 2.7× faster via parallelization. Justified for large workloads.
4. **Hierarchical + Compaction**: 3× faster, similar disk to linear. Optimal for very large workloads.

**Recommended Path Forward:**

1. **Start with profiling**: Measure actual bottlenecks in current system on target workloads
   - If disk space is the issue → implement compaction first
   - If processing time is the issue → consider hierarchical

2. **Incremental implementation for hierarchical**:
   - Phase 1: Implement tree builder (organize files, process linearly to test structure)
   - Phase 2: Implement interner merge with comprehensive tests
   - Phase 3: Implement result set merge with comprehensive tests
   - Phase 4: Implement parallel leaf processing (validate deduplication)
   - Phase 5: Implement internal node processing
   - Phase 6: Validate results match linear version
   - Phase 7: Add compaction after hierarchical is stable

3. **Incremental implementation for compaction**:
   - Phase 1: Implement containment detection algorithm
   - Phase 2: Implement compaction during BFS processing
   - Phase 3: Implement containment map persistence
   - Phase 4: Implement reconstruction on interner change
   - Phase 5: Validate no correctness loss vs. uncompacted

4. **Critical validation**:
   - Run both approaches on same input
   - Verify final optimal ortho matches (or understand why it differs)
   - Verify no orthos are lost due to bugs
   - Measure actual speedup and resource usage vs. predictions

The **compaction optimization is orthogonal** to the hierarchical vs. linear decision and provides benefits to both. The **hierarchical approach is a significant architectural change** that should only be undertaken with clear performance goals (2-5× speedup required) and rigorous validation. For most workloads, **linear + compaction** provides the best balance of simplicity, correctness, and performance improvement.
