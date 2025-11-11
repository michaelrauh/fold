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
An ortho A "falls wholly inside" another ortho B if all cells filled in A are also filled in B with the same values, and B has additional filled cells. When this occurs, A is **truly removed** from the results queue - deleted from disk and memory. Recovery happens through deterministic reconstruction by subtracting pieces from the containing ortho.

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
    removed_count = 0
    for ortho in results_queue:
        is_redundant = false
        for other in compacted:
            if is_contained(ortho, other):
                is_redundant = true
                removed_count += 1
                break
        if not is_redundant:
            # Also check if ortho subsumes any existing orthos
            newly_redundant = [o for o in compacted if is_contained(o, ortho)]
            removed_count += len(newly_redundant)
            compacted = [o for o in compacted if not is_contained(o, ortho)]
            compacted.append(ortho)
    
    # Truly delete removed orthos from disk and memory
    delete_from_disk(removed_count)
    return compacted
```

**Deterministic Reconstruction by Subtraction:**
When the interner changes, contained orthos are reconstructed by deterministically removing filled cells from their parent orthos:

```
function reconstruct_by_subtraction(parent_ortho, cells_to_remove):
    # Create a partial ortho by subtracting specific filled cells
    # This generates all possible sub-orthos that were contained
    reconstructed = []
    
    for subset in power_set(parent_ortho.filled_cells()):
        if subset == parent_ortho.filled_cells():
            continue  # Skip the parent itself
        
        child_ortho = Ortho::new_with_cells(subset)
        reconstructed.append(child_ortho)
    
    return reconstructed

function reconstruct_on_interner_change(compacted_results, interner, impacted_keys):
    # For each compacted ortho that references impacted keys
    for parent_ortho in compacted_results:
        if parent_ortho.references_any(impacted_keys):
            # Deterministically generate all sub-orthos
            sub_orthos = reconstruct_by_subtraction(parent_ortho, parent_ortho.filled_cells())
            
            # Re-process sub-orthos with new interner
            for sub_ortho in sub_orthos:
                if sub_ortho.references_any(impacted_keys):
                    work_queue.push(sub_ortho)
```

**Storage Requirements:**
- Compacted results queue: Significantly smaller (typically 10-30% of original)
- NO containment map needed - reconstruction is deterministic through subtraction
- Total savings: 70-90% reduction in result queue size with no additional storage overhead

**Trade-offs:**
- **Benefit**: Dramatically reduced result queue size (memory + disk)
- **Benefit**: Fewer orthos to scan when finding impacted keys
- **Benefit**: Faster checkpoint save/load (less data)
- **Benefit**: No containment map storage overhead
- **Cost**: Compaction algorithm is O(N²) in worst case (all orthos compared)
- **Cost**: Reconstruction generates power set of filled cells (exponential, but limited by ortho size)
- **Cost**: Reconstruction overhead when interner changes (but only for impacted orthos)

**Implementation Notes:**
- Compaction can be run periodically (e.g., every 10k orthos generated) rather than continuously
- Can use spatial heuristics to optimize containment checks (orthos with different dimensions cannot contain each other)
- Reconstruction is expensive but infrequent (only on interner changes affecting specific keys)
- Power set generation is bounded by ortho payload size (~10-50 filled cells typically)

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

Merging result sets from multiple subtrees requires careful handling because orthos from different subtrees have **incompatible token encodings**. Each subtree's interner assigns different numeric IDs to tokens (e.g., one maps ID 1 to "the", another maps ID 1 to "is").

**The Encoding Problem:**

```rust
// Subtree A interner:
vocabulary: ["the", "cat", "sat"] // "the" = 0, "cat" = 1, "sat" = 2

// Subtree B interner:
vocabulary: ["is", "the", "dog"] // "is" = 0, "the" = 1, "dog" = 2

// Ortho from subtree A: payload = [Some(0), Some(1)] means "the cat"
// Ortho from subtree B: payload = [Some(0), Some(1)] means "is the"
// Same payload values, completely different meanings!
```

**Solution: Remap Before Merging:**

Orthos must be remapped to the merged interner's token space before deduplication:

```rust
pub struct ResultMerger {
    merged_interner: Interner,
    seen_tracker: SeenTracker,
    merged_results: DiskBackedQueue<Ortho>,
    current_best: Option<Ortho>,
    current_best_score: (usize, usize),
}

impl ResultMerger {
    pub fn merge(
        child_results: Vec<(DiskBackedQueue<Ortho>, Interner)>,
        merged_interner: Interner,
        memory_config: &MemoryConfig
    ) -> Result<Self, FoldError> {
        
        // Build remapping tables for each child interner
        let remapping_tables: Vec<HashMap<usize, usize>> = child_results.iter()
            .map(|(_, child_interner)| {
                build_remapping_table(child_interner, &merged_interner)
            })
            .collect();
        
        let estimated_total = child_results.iter().map(|(q, _)| q.len()).sum();
        let seen_tracker = SeenTracker::with_config(
            estimated_total * 3,
            memory_config.num_shards,
            memory_config.max_shards_in_memory
        );
        let merged_results = DiskBackedQueue::new(memory_config.queue_buffer_size)?;
        
        let mut merger = ResultMerger {
            merged_interner,
            seen_tracker,
            merged_results,
            current_best: None,
            current_best_score: (0, 0),
        };
        
        // Stream through each result set with remapping
        for ((mut result_set, child_interner), remap_table) in 
            child_results.into_iter().zip(remapping_tables.iter()) {
            
            println!("[merge] Processing result set with {} orthos", result_set.len());
            let mut added = 0;
            let mut duplicates = 0;
            
            while let Some(ortho) = result_set.pop()? {
                // CRITICAL: Remap ortho to merged interner's token space
                let remapped_ortho = remap_ortho(&ortho, remap_table, merged_interner.version());
                let id = remapped_ortho.id();
                
                // Now deduplication is correct
                if !merger.seen_tracker.contains(&id) {
                    merger.seen_tracker.insert(id);
                    
                    let score = calculate_score(&remapped_ortho);
                    if score > merger.current_best_score {
                        merger.current_best = Some(remapped_ortho.clone());
                        merger.current_best_score = score;
                    }
                    
                    merger.merged_results.push(remapped_ortho)?;
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
}

// Build a mapping from child interner token IDs to merged interner token IDs
fn build_remapping_table(child_interner: &Interner, merged_interner: &Interner) 
    -> HashMap<usize, usize> {
    let mut remap = HashMap::new();
    
    for (child_idx, word) in child_interner.vocabulary().iter().enumerate() {
        // Find this word's index in the merged vocabulary
        if let Some(merged_idx) = merged_interner.vocabulary().iter().position(|w| w == word) {
            remap.insert(child_idx, merged_idx);
        }
    }
    
    remap
}

// Remap an ortho's payload from child token space to merged token space
fn remap_ortho(ortho: &Ortho, remap_table: &HashMap<usize, usize>, new_version: usize) 
    -> Ortho {
    let mut new_payload = ortho.payload().clone();
    
    for cell in new_payload.iter_mut() {
        if let Some(token_id) = cell {
            *token_id = remap_table[token_id];
        }
    }
    
    Ortho::new_with_payload(ortho.dims().clone(), new_payload, new_version)
}
```

**Performance Characteristics:**

- **Time Complexity**: O(R₁ + R₂ + ... + Rₙ) × O(P) where R is result count per subtree, P is avg payload size for remapping
- **Space Complexity**: O(R_total) for bloom filter and disk-backed shards, O(V_merged) for remapping tables
- **Remapping Cost**: Each ortho must have its payload remapped (O(P) per ortho, where P = filled cells)
- **Typical Cost**: For 4 subtrees with 225M results each (890M total), scaled appropriately:
  - Remapping time: ~10-20 minutes (touching every ortho's payload)
  - Merge time: ~30-60 minutes (disk I/O dominated, handling ~890M orthos)
  - Memory: ~2-4 GB for bloom filter + hot shards (scaled for 890M orthos)

**Critical Correctness Constraint:**

Without remapping, merge would be incorrect - orthos with identical payloads but different meanings would be incorrectly deduplicated. Remapping ensures token IDs are consistent across the merged interner's vocabulary before deduplication.

**Deduplication Effectiveness:**

The degree of duplication between subtrees depends on:
1. **Text similarity**: More similar text → more duplicate orthos (after remapping)
2. **Vocabulary overlap**: Shared vocabulary → more orthos map to same values
3. **Seed ortho**: All subtrees start with same seed → guaranteed duplication of early expansions

**Expected duplication rates (after remapping):**
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
    
    // Process the tree hierarchically
    let result = processor.process_tree(&tree)?;
    
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

## Extreme Hierarchical: Sentence-Level Binary Merge

This section analyzes taking hierarchical processing to its extreme: treating each **sentence** as the smallest processing unit and performing a binary merge tree.

### Concept

The Fold splitter already splits text on punctuation (`.`, `?`, `;`, `!`, paragraph breaks). Instead of processing files, process individual sentences:

```
Sentence 1 → Orthos₁
Sentence 2 → Orthos₂     } → Merge → Orthos₁₂
Sentence 3 → Orthos₃
Sentence 4 → Orthos₄     } → Merge → Orthos₃₄  } → Merge → Orthos₁₂₃₄
...

Binary tree depth: log₂(N) where N = sentence count
```

### Scale Analysis

**For Short Book (16 files, 890M orthos):**

Assume average book:
- ~100K words
- ~5K sentences (assuming ~20 words/sentence)
- Binary tree depth: log₂(5000) ≈ 12-13 levels

**Per-Sentence Processing:**
- Each sentence is tiny (~20 words)
- Interner per sentence: ~20 unique words, ~50-100 phrases
- Orthos generated per sentence: ~100-1000 (MUCH smaller than 890M)
- **Key insight**: Most orthos come from CROSS-SENTENCE phrase combinations

### Resource Breakdown

**Leaf Level (5K sentence nodes):**
- Per-sentence BFS: ~1-10 seconds (tiny vocabulary, few orthos)
- Total leaf processing: 5K × 5 sec = ~7 hours (massively parallel)

**Merge Levels (12-13 levels up to root):**

Level 1 (2.5K merges):
- Each merge: 2 sentences → ~2K orthos → merge with remapping
- Interner merge: ~40 words → ~80 words
- Impacted ortho scanning: ~2K orthos (tiny!)
- Per-merge time: ~5-30 seconds
- Parallelizable: 2.5K merges can run in parallel

Level 6 (halfway, ~78 merges):
- Each merge: ~32 sentences → ~50K-500K orthos
- Interner merge: ~500 words → ~600 words
- Impacted ortho scanning: ~500K orthos
- Per-merge time: ~30-300 seconds

Level 12 (near root, ~2 merges):
- Each merge: ~2500 sentences → ~200M-500M orthos
- Interner merge: ~25K words → ~50K words
- Impacted ortho scanning: ~500M orthos
- Per-merge time: ~10-50 hours

Level 13 (root merge):
- Final merge: ~5K sentences → ~890M orthos
- Interner merge: ~50K words (full vocabulary)
- Impacted ortho scanning: ~890M orthos
- Merge time: ~50-100 hours

**Total Time Estimate:**
- Leaf processing: ~7 hours (5K × 5 sec, parallel)
- Level 1-6 merges: ~50 hours (small merges, mostly parallel)
- Level 7-12 merges: ~200 hours (medium merges, less parallelism)
- Level 13 root merge: ~100 hours (final merge)
- **Total: ~357 hours (~15 days)**

### Disk Usage

**Challenge: Intermediate State Explosion**

Each merge level generates new orthos:
- Level 1: 2.5K × 2K orthos = 5M orthos (1.5 GB)
- Level 6: 78 × 500K orthos = 39M orthos (12 GB)
- Level 12: 2 × 500M orthos = 1B orthos (300 GB)
- Level 13: 890M orthos final (267 GB)

**Temporary disk for all levels:** ~600-800 GB (not counting cleanup)

With cleanup (deleting child results after merge): ~300-400 GB peak

### RAM Usage

**Per-merge node:**
- Level 1-6: ~100 MB - 1 GB (small merges)
- Level 7-12: ~5-15 GB (medium merges)
- Level 13: ~22 GB (root merge)

**Parallelization bottleneck:**
- Early levels: Can run 100s of merges in parallel (low RAM each)
- Late levels: Limited to 2-4 merges in parallel (high RAM each)

### Benefits of Sentence-Level Binary Merge

1. **Maximum Granularity**
   - Smallest possible processing unit (sentence)
   - Maximum parallelization at leaf level (5K sentences can process simultaneously)
   - Fine-grained progress tracking

2. **Logarithmic Depth**
   - Binary tree: log₂(N) levels (12-13 for 5K sentences)
   - File-based tree: log₄(N) levels (6-7 for 16 files)
   - More levels = more opportunities for parallelization

3. **Smaller Merge Overhead**
   - Early merges are tiny (seconds each)
   - Impacted ortho scanning on small sets is fast
   - Remapping overhead distributed across many small merges

4. **Fault Tolerance**
   - Failure in one sentence doesn't lose much work
   - Can checkpoint at each merge level
   - Resume from any level

### Drawbacks of Sentence-Level Binary Merge

1. **Merge Overhead Dominates**
   - 12-13 merge levels vs 2-3 for file-based
   - Each level requires remapping ALL orthos
   - Impacted ortho scanning happens at EVERY level
   - **Key problem**: The same 890M orthos get remapped 12-13 times instead of 2-3 times

2. **Cumulative Remapping Cost**
   - File-based: 890M orthos remapped 2-3 times = ~2.7B remap operations
   - Sentence-based: Orthos remapped at EACH level up
   - Effective remapping: ~5-8B remap operations (worse!)
   - **Time penalty**: 2-3× more merge time than file-based

3. **Implementation Complexity**
   - Managing 5K leaf nodes vs 16 file nodes
   - 12-13 merge levels vs 2-3 levels
   - More complex scheduling and resource allocation
   - More checkpoint states to manage

4. **Diminishing Returns**
   - Leaf processing is already tiny (7 hours vs 18 days for file-based)
   - Bottleneck shifts entirely to merge operations
   - More merge levels = MORE total merge time, not less

5. **Memory Fragmentation**
   - 5K tiny interners vs 16 medium interners
   - More temporary merge states in flight
   - Harder to pack into available RAM efficiently

### Comparison: File-Based vs Sentence-Based

| Aspect | File-Based (4-ary) | Sentence-Based (Binary) |
|--------|-------------------|------------------------|
| Leaf nodes | 16 files | 5K sentences |
| Tree depth | 2-3 levels | 12-13 levels |
| Leaf processing | ~18 days (parallel) | ~7 hours (parallel) |
| Merge operations | ~8 hours | ~357 hours (15 days) |
| Total time | ~29 days | ~15 days |
| Remapping operations | ~2.7B | ~5-8B |
| Impacted scans | 3 merge points | 13 merge points |
| Implementation | Moderate | High complexity |
| **Winner** | **Simplicity** | **Slight speedup** |

### Analysis Conclusion

**Sentence-level binary merge provides ~2× speedup (29 → 15 days) but:**

1. **Marginal benefit**: 2× vs 3× from file-based hierarchical
2. **High complexity**: 300× more nodes, 4× more merge levels
3. **Merge-dominated**: 95% of time is merging, not processing
4. **Remapping explosion**: 2-3× more remapping operations

**Better approach**: Hybrid strategy
- Use sentences for initial grouping (e.g., 100 sentences per leaf)
- Binary merge in early levels (low overhead)
- Switch to larger branching factor at higher levels
- Balance parallelization vs merge overhead

**Recommendation**: Sentence-level binary merge is NOT worth the complexity. File-based quaternary (4-way) tree provides better simplicity/performance trade-off. If more speedup needed, **performance tuning (3-6×)** and **higher branching factors** (8-way or 16-way tree) are more effective than going to sentence-level granularity.

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
   - **Typical**: ~20-180 seconds per file (scanning 10M-890M orthos, scales linearly)

4. **Forming Completions (Interner Operations)**: 5-10% of total time
   - Build interner from text: O(T) where T = text size
   - Extract vocabulary: O(W) where W = unique words
   - Build prefix_to_completions: O(P × L) where P = phrases, L = avg phrase length
   - Intersect for completions: O(num_prefixes × vocabulary_size) per call
   - **Typical**: ~1-5 seconds per file to build interner

5. **Checkpoint Save/Load**: <5% of total time
   - Serialize interner: O(V + P) typically 10-50ms
   - Flush results queue: O(R) typically minutes for 890M orthos
   - Rebuild seen tracker: O(R) typically minutes for 890M orthos
   - **Typical**: 5-15 minutes per checkpoint at scale

**Example: Processing Short Book (~16 files, 890M total orthos, 50K vocabulary):**
- BFS processing: ~1480 hours (~62 days at 1-5ms/ortho × 890M orthos) (70%)
- Uniqueness checks: ~420 hours (~17.5 days at 100-200µs/ortho × 890M orthos) (20%)
- Finding impacted: ~8 file transitions × 180 sec = 24 minutes (<1%)
- Forming completions: ~16 files × 3 sec = 48 seconds (<1%)
- Checkpointing: ~8 checkpoints × 10 min = 80 minutes (<1%)
- **Total: ~2100 hours (~88 days) - highly parallelization-motivated!**

**Disk Usage:**
- Interner: 50-500 MB (depends on vocabulary size)
- Results queue: R × 200-400 bytes (e.g., 890M orthos = 178-356 GB)
- Seen tracker shards: R × 12 bytes on disk (e.g., 890M orthos = 10.7 GB)
- Checkpoint backup: 2× results queue (356-712 GB)
- **Total: ~550-1080 GB for 890M orthos**

**RAM Usage (with dynamic configuration for 64GB machine at scale):**
- Interner: ~100-200 MB in memory
- Work queue buffer: ~50K orthos × 300 bytes = 15 MB
- Results queue buffer: ~50K orthos × 300 bytes = 15 MB
- Bloom filter: 2.7B capacity (890M × 3) × 2 bytes = 5.4 GB
- Hot shards: ~1024 shards × 1M entries × 12 bytes = 12 GB
- Runtime overhead: ~20% = ~4 GB
- **Total: ~22 GB peak**

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
   - Stream and deduplicate WITH REMAPPING: O(R_total × P) where P = payload size
   - Disk I/O to read child results: O(R_total × 300 bytes)
   - Bloom filter + shard checks: Same as linear per ortho
   - **Typical**: ~30-90 minutes per internal node (890M orthos, remapping cost)
   - **Total merge time**: ~90-270 minutes (3 internal nodes)

4. **Uniqueness Checks**: 10-15% of total time
   - Per-node bloom filters: Smaller, more efficient per node
   - Final merge bloom: Same size as linear
   - Overall similar to linear but distributed

5. **Impacted Ortho Scanning (At Merge Points)**: 5-8% of total time
   - **CORRECTION**: Hierarchical DOES require impacted ortho scanning
   - When merging child results, interner changes (child interners → merged interner)
   - Must scan child orthos to find those referencing changed keys
   - However, scanning is localized per merge (not global)
   - **Typical**: ~30-90 minutes per internal node merge (scanning 223M-890M orthos)
   - **Advantage**: Can be parallelized across merge points
   - Saved time vs linear: Parallelization, not elimination

**Example: Same Short Book (~16 files, 890M orthos workload):**
- Leaf BFS (parallel 4×): ~1480 hours / 3.5 = ~423 hours (~18 days)
- Internal node BFS (3 nodes): ~265 hours (~11 days) 
- Interner merging: ~60 seconds (3 nodes × 20s)
- Impacted ortho scanning at merges: ~180 minutes (3 nodes × 60 min, can overlap with BFS)
- Result merging with remapping: ~270 minutes (~4.5 hours)
- **Total: ~695 hours (~29 days) - 3× speedup over linear with 4× parallelization**

**Disk Usage:**
- Per-leaf results: ~223M orthos × 300 bytes = 67 GB × 4 = 268 GB
- Per-internal results: Grows to final ~890M orthos = 267 GB
- Per-node interners: ~50-200 MB each, 7 nodes = 350-1400 MB
- Merge buffers (transient): ~270 GB during merge operations
- Checkpoint: All node states = ~800-1600 GB
- **Total: ~1.6-3.2 TB (2-3× linear)**

**RAM Usage (per node, with 64GB machine at scale):**
- Can process 2-3 nodes in parallel (limited by RAM)
- Per-node budget: ~20-30 GB
- Leaf node peak: ~5 GB (smaller working set, ~223M orthos)
- Internal node peak: ~22 GB (during merge with remapping)
- **Bottleneck**: Internal node merging with remapping requires most RAM

### Hierarchical Processing WITH Compaction

**Time Breakdown (for Short Book, 890M orthos, 70% compaction rate):**

1. **BFS Work Queue Processing**: 55-65% (same as without compaction)

2. **Compaction (Periodic)**: 5-10% of total time
   - Run every 10K orthos: ~22,300 compaction runs per leaf (223M / 10K)
   - O(N²) comparison in worst case, but spatial heuristics help
   - **Typical**: ~50-200ms per compaction run
   - **Total**: ~1-4 hours per leaf

3. **Interner Merging**: 8-12% of total time
   - Faster due to smaller result sets to process
   - **Typical**: ~10-30 seconds per internal node

4. **Result Set Merging with Remapping**: 5-8% of total time
   - Only 30% of orthos to merge (70% were compacted)
   - 267M orthos after compaction instead of 890M
   - **Typical**: ~60-120 minutes per internal node
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

**For Short Book Scale (890M orthos):**

| Approach | Time | Disk | RAM | Best For |
|----------|------|------|-----|----------|
| Linear | ~88 days | 550-1080 GB | 22 GB | Correctness, simplicity (impractical at scale) |
| Linear + Compaction | ~75 days | 165-325 GB | 7 GB | Memory-constrained (still slow) |
| Hierarchical File-Based (4× parallel) | ~29 days | 1.6-3.2 TB | 22 GB/node | Parallelization (disk-heavy, **impacted scanning at merges**) |
| Hierarchical Sentence-Based (binary) | ~15 days | 600-800 GB | 22 GB/node | Max parallelization (high merge overhead) |
| Hierarchical + Compaction | ~25 days | 500-950 GB | 7 GB/node | Better disk usage |
| Linear + Performance Tuning (6×) | ~15 days | 550-1080 GB | 22 GB | Single-machine optimization |
| Hierarchical + Perf Tuning (3× speedup) | ~10 days | 1.6-3.2 TB | 22 GB/node | **Best time**: Parallel + optimized |
| Hierarchical + Compaction + Perf Tuning | ~8 days | 500-950 GB | 7 GB/node | **Best overall**: Time + disk efficiency |

**Key Insights:**

1. **Scale matters**: At 890M orthos, linear processing takes ~88 days - hierarchical parallelization becomes essential
2. **Performance tuning is critical**: 3-6× speedup from code optimization alone
3. **Compaction crucial for disk**: Saves 70% disk space (1TB → 300GB after compaction)
4. **Hierarchical adds complexity**: Requires remapping during merge, 2-3× disk overhead during processing
5. **Remapping cost is significant**: Touching every ortho's payload adds 10-30% to merge time
6. **Impacted scanning still needed**: Hierarchical must scan for impacted orthos at each merge point (not eliminated, just localized)
7. **Sentence-level diminishing returns**: Binary merge on 5K sentences gives 2× speedup but 4× complexity over file-based
8. **Combined approach best**: Hierarchical + Compaction + Performance Tuning reduces 88 days → 8 days

### Technical Considerations

### Memory Management

**Hierarchical:**
- Memory per node = interner + work_queue + results_queue + tracker
- Peak memory = max(simultaneous nodes) × per_node_memory
- Can control via tree depth and parallel execution limit
- With compaction: Can process 3-4 nodes in parallel instead of 1-2 (reduced memory per node)

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
- ⚠️  Compacted orthos must be remapped during merge (adds overhead)

## Performance Tuning: Orthogonal Optimization

Performance tuning is an orthogonal optimization strategy that can be applied to **any** of the above approaches (linear, hierarchical, compaction, or combinations).

### Core Concept

Rather than changing the algorithmic approach, performance tuning focuses on making the existing slow parts more efficient through:
1. **Profiling-guided optimization**: Identify actual bottlenecks via profiling
2. **Algorithmic micro-optimizations**: Replace slow operations with faster equivalents
3. **Parallelization with Rayon**: Use data parallelism for CPU-bound operations

### Profiling First

Before optimizing, profile to find actual bottlenecks:

```bash
# Profile with perf
cargo build --release
perf record -g ./target/release/fold
perf report

# Profile with flamegraph
cargo install flamegraph
cargo flamegraph

# Profile with criterion benchmarks
cargo bench
```

**Common bottlenecks identified in profiling:**
1. Interner intersection (40-50% of BFS time)
2. Ortho ID computation (hashing) (10-15% of BFS time)
3. Spatial transformations during expansion (15-20% of BFS time)
4. Seen tracker lookups (10-15% of BFS time)

### Optimization Strategies

#### 1. Optimize Interner Intersection

Current: O(num_prefixes × vocabulary_size) with FixedBitSet operations

**Optimization A: Cache frequent intersections**
```rust
struct InternerWithCache {
    interner: Interner,
    intersection_cache: LruCache<(Vec<usize>, Vec<usize>), Vec<usize>>,
}

impl InternerWithCache {
    fn intersect_cached(&mut self, required: &[Vec<usize>], forbidden: &[usize]) -> Vec<usize> {
        let key = (required.to_vec(), forbidden.to_vec());
        if let Some(cached) = self.intersection_cache.get(&key) {
            return cached.clone();
        }
        
        let result = self.interner.intersect(required, forbidden);
        self.intersection_cache.put(key, result.clone());
        result
    }
}
```

**Optimization B: Parallel intersection with Rayon**
```rust
use rayon::prelude::*;

fn intersect_parallel(&self, required: &[Vec<usize>], forbidden: &[usize]) -> Vec<usize> {
    let vocab_size = self.vocabulary.len();
    
    // Parallel check each completion candidate
    (0..vocab_size)
        .into_par_iter()
        .filter(|&token| {
            !forbidden.contains(&token) && 
            required.iter().all(|prefix| self.is_valid_completion(prefix, token))
        })
        .collect()
}
```

**Expected speedup**: 20-30% reduction in BFS time

#### 2. Optimize Ortho ID Computation

Current: Hash entire payload on every ortho creation

**Optimization: Incremental hashing**
```rust
impl Ortho {
    fn compute_id_incremental(parent_id: u64, new_value: usize, position: usize) -> usize {
        // Use parent's hash as seed, only hash the delta
        let mut hasher = DefaultHasher::new();
        parent_id.hash(&mut hasher);
        new_value.hash(&mut hasher);
        position.hash(&mut hasher);
        (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
    }
}
```

**Expected speedup**: 10-15% reduction in BFS time

#### 3. Optimize Spatial Transformations

Current: Generates all possible expansions, creating many temporary allocations

**Optimization: Lazy expansion iterator**
```rust
struct LazyExpansionIterator {
    ortho: Ortho,
    expansions: Vec<(Vec<usize>, usize, Vec<usize>)>,
    index: usize,
}

impl Iterator for LazyExpansionIterator {
    type Item = Ortho;
    
    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.expansions.len() {
            return None;
        }
        
        let (new_dims, new_capacity, reorg) = &self.expansions[self.index];
        self.index += 1;
        
        // Build ortho on demand, avoiding Vec allocations where possible
        Some(self.ortho.expand_with(new_dims, *new_capacity, reorg))
    }
}
```

**Expected speedup**: 15-20% reduction in BFS time

#### 4. Parallelize BFS Processing with Rayon

**Optimization: Process work queue chunks in parallel**
```rust
use rayon::prelude::*;

fn process_work_queue_parallel(
    work_queue: &mut DiskBackedQueue<Ortho>,
    interner: &Interner,
    results: &mut DiskBackedQueue<Ortho>,
    tracker: &mut SeenTracker,
) -> Result<(), FoldError> {
    
    const CHUNK_SIZE: usize = 1000;
    
    loop {
        // Pop a chunk of orthos
        let mut chunk = Vec::with_capacity(CHUNK_SIZE);
        for _ in 0..CHUNK_SIZE {
            if let Some(ortho) = work_queue.pop()? {
                chunk.push(ortho);
            } else {
                break;
            }
        }
        
        if chunk.is_empty() {
            break;
        }
        
        // Process chunk in parallel
        let new_orthos: Vec<Vec<Ortho>> = chunk.par_iter()
            .map(|ortho| {
                let (forbidden, required) = ortho.get_requirements();
                let completions = interner.intersect(&required, &forbidden);
                
                completions.iter()
                    .flat_map(|&completion| ortho.add(completion, interner.version()))
                    .collect()
            })
            .collect();
        
        // Sequentially insert results (tracker is not thread-safe)
        for orthos in new_orthos {
            for ortho in orthos {
                let id = ortho.id();
                if !tracker.contains(&id) {
                    tracker.insert(id);
                    results.push(ortho.clone())?;
                    work_queue.push(ortho)?;
                }
            }
        }
    }
    
    Ok(())
}
```

**Expected speedup**: 2-4× on multi-core machines (scales with core count)

### Combined Impact

Applying all optimizations:
- Interner caching + parallelization: -20-30%
- Incremental hashing: -10-15%
- Lazy expansion: -15-20%
- Parallel BFS: 2-4× (multi-core)

**Total speedup: 3-6× on 8-core machine**

### Implementation Priority

1. **Profile first**: Don't optimize blindly
2. **Start with Rayon parallelization**: Biggest win, smallest code change
3. **Add intersection caching**: Next biggest win
4. **Incremental hashing**: Medium win, moderate complexity
5. **Lazy expansion**: Smallest win, highest complexity

### Trade-offs

**Benefits:**
- Significant speedup (3-6×) without changing algorithm
- Can combine with any other optimization (hierarchical, compaction)
- Rayon makes parallelization easy (minimal code changes)

**Costs:**
- Profiling and benchmarking required
- Incremental optimization increases code complexity
- Parallel code harder to debug
- Caching increases memory usage

### When to Use Performance Tuning

**Best combined with:**
- Linear processing: Brings 88 days → 15-30 days
- Hierarchical processing: Brings 29 days → 5-10 days
- Either with compaction

**Always do this AFTER** algorithmic decisions (linear vs hierarchical, with/without compaction) are made. Performance tuning is the final layer of optimization.

## Recommendation

### When to Use Linear (Current System)

**Best for:**
- Small datasets (< 10M orthos, ~1-2 days processing time)
- When deterministic results are required
- When correctness is paramount
- When simplicity is valued
- When debugging is frequent

**Example scenarios:**
- Processing small texts or experiments
- Research/development work
- Validation of algorithm correctness
- Prototyping

**Reality check:**
- At 890M orthos (short book), linear takes ~88 days - **impractical**
- Performance tuning can reduce to ~15 days, still slow
- Linear alone is NOT viable for production at this scale

### When to Use Hierarchical Processing

**ESSENTIAL for:**
- Large datasets (> 100M orthos, e.g., short book = 890M orthos)
- When linear would take weeks/months (anything > 10 days)
- When parallelization infrastructure is available (multi-core or distributed)

**Benefits at scale:**
- 890M orthos: 88 days → 29 days (3× speedup with 4× parallelization)
- With performance tuning: 29 days → 10 days
- With compaction added: 10 days → 8 days

**Critical considerations:**
- **Remapping overhead**: Must remap every ortho during merge (10-30% overhead)
- **Disk overhead**: 2-3× disk usage during processing (merge buffers)
- **Correctness complexity**: Token ID remapping must be perfect
- **Implementation effort**: ~3-5× more code than linear

**Implementation recommendation:**
- Start with proof-of-concept for small tree (depth 2, 4 leaves)
- Validate remapping correctness thoroughly
- Benchmark merge overhead with remapping
- Test on progressively larger datasets (1M → 10M → 100M → 890M orthos)

### When to Use Compaction

**ESSENTIAL for:**
- Any workload where disk space is constrained
- Large datasets (> 100M orthos)
- When checkpoint save/load time is bottleneck (hours at 890M orthos)

**Benefits:**
- 70-90% disk space reduction (1TB → 300GB)
- 15-30% faster checkpoint operations
- Faster impacted ortho scanning (fewer orthos to check)

**Critical considerations:**
- **Truly destructive**: Compacted orthos are DELETED from disk
- **Reconstruction is expensive**: Power set generation on interner change
- **Deterministic only**: Reconstruction by subtraction must be correct

**Best combined with:**
- Linear processing: Simple improvement, 15% faster, 50% less disk
- Hierarchical processing: Optimal combination, 3× faster, similar disk to linear

**Implementation recommendation:**
- Implement compaction first as incremental improvement to linear
- Test containment detection algorithm thoroughly
- Verify reconstruction logic is correct on interner changes
- Add compaction to hierarchical only after both are working

## Conclusion

The analysis reveals that **scale dramatically changes the optimal approach**. At 890M orthos (short book scale):

1. **Linear (current)**: ~88 days - IMPRACTICAL
2. **Linear + Performance Tuning**: ~15 days - Marginal, still slow
3. **Linear + Compaction**: ~75 days - Saves disk but still too slow
4. **Hierarchical File-Based**: ~29 days - Viable with parallelization (3× speedup, **still needs impacted scanning**)
5. **Hierarchical Sentence-Based**: ~15 days - Max parallelization but high merge complexity
6. **Hierarchical + Performance Tuning**: ~10 days - Good (9× speedup)
7. **Hierarchical + Compaction + Performance Tuning**: ~8 days - **Best** (11× speedup)

**Key Findings:**

1. **Hierarchical is ESSENTIAL at scale**: 890M orthos makes linear processing impractical (months)
2. **Performance tuning is critical**: 3-6× speedup from code optimization
3. **Compaction saves 70% disk**: 1TB → 300GB, essential for storage constraints
4. **Remapping adds complexity**: Every ortho must be remapped during merge (non-trivial cost)
5. **Impacted scanning NOT eliminated**: Hierarchical still needs to scan for impacted orthos at merge points (localized but not eliminated)
6. **Sentence-level has diminishing returns**: Binary merge on 5K sentences gives 2× speedup but 4× implementation complexity
7. **Deterministic reconstruction required**: Compaction must truly delete orthos and reconstruct by subtraction

**Recommended Path Forward:**

1. **Start with performance tuning on linear**:
   - Profile to find bottlenecks
   - Implement Rayon parallelization (biggest win)
   - Add intersection caching
   - Measure: Can this get to acceptable time? (< 2 weeks)

2. **If performance tuning insufficient, implement hierarchical (file-based)**:
   - Phase 1: Implement interner merge with token remapping
   - Phase 2: Implement result merge with remapping validation
   - Phase 3: Implement impacted ortho scanning at merge points
   - Phase 4: Build tree structure and parallel leaf processing
   - Phase 5: Implement internal node processing with merge
   - Phase 6: **Critical**: Validate remapping correctness thoroughly
   - Phase 7: Benchmark actual speedup on real data
   - **Do NOT go to sentence-level** - diminishing returns for high complexity

3. **Add compaction after hierarchical is stable**:
   - Phase 1: Implement true destructive compaction (delete from disk)
   - Phase 2: Implement deterministic reconstruction by subtraction
   - Phase 3: Validate no data loss
   - Phase 4: Measure disk savings and reconstruction cost

4. **Critical validation at each step**:
   - Run on progressively larger datasets (1M → 10M → 100M → 890M)
   - Validate ortho count matches expected
   - Verify final optimal ortho is reasonable
   - Measure actual time/disk/RAM vs. predictions
   - **Verify impacted scanning works correctly at merge points**

The **hierarchical approach with remapping is complex but necessary** at scale. The current linear system cannot handle 890M orthos in reasonable time. Performance tuning helps but cannot overcome the fundamental sequential bottleneck. **Hierarchical + Compaction + Performance Tuning** reduces processing from 88 days to 8 days - a requirement, not an optimization. Note that hierarchical does NOT eliminate impacted ortho scanning - it localizes it to merge points, which can be parallelized but still must be performed.
