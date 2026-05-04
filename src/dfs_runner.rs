use crate::{
    completion_pruning::{
        CompletionContext, completion_upper_bound_ctx, existing_ortho_upper_bound_ctx,
    },
    error::FoldError,
    interner::Interner,
    ortho::{Ortho, OrthoScore, PayloadVal},
};
use bytecheck::CheckBytes;
use fixedbitset::FixedBitSet;
use rayon::prelude::*;
use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct StepScratch {
    completion_ctx: CompletionContext,
    frame_ctx: CompletionContext,
    completion_bits: FixedBitSet,
}

thread_local! {
    static STEP_SCRATCH: RefCell<StepScratch> = RefCell::new(StepScratch::default());
}

#[derive(Clone, Debug, SerdeSerialize, SerdeDeserialize)]
pub struct DfsConfig {
    pub checkpoint_every_nodes: u64,
    pub checkpoint_every_secs: u64,
    pub metrics_every_nodes: u64,
    pub max_frame_branch_cache: Option<usize>,
}

impl Default for DfsConfig {
    fn default() -> Self {
        Self {
            checkpoint_every_nodes: 50_000,
            checkpoint_every_secs: 30,
            metrics_every_nodes: 2_000,
            max_frame_branch_cache: None,
        }
    }
}

impl DfsConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(value) = std::env::var("FOLD_CHECKPOINT_EVERY_NODES") {
            if let Ok(parsed) = value.parse() {
                cfg.checkpoint_every_nodes = parsed;
            }
        }
        if let Ok(value) = std::env::var("FOLD_CHECKPOINT_EVERY_SECS") {
            if let Ok(parsed) = value.parse() {
                cfg.checkpoint_every_secs = parsed;
            }
        }
        if let Ok(value) = std::env::var("FOLD_METRICS_EVERY_NODES") {
            if let Ok(parsed) = value.parse() {
                cfg.metrics_every_nodes = parsed;
            }
        }
        if let Ok(value) = std::env::var("FOLD_MAX_FRAME_BRANCH_CACHE") {
            if let Ok(parsed) = value.parse() {
                cfg.max_frame_branch_cache = Some(parsed);
            }
        }
        cfg
    }

    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = rustc_hash::FxHasher::default();
        self.checkpoint_every_nodes.hash(&mut hasher);
        self.checkpoint_every_secs.hash(&mut hasher);
        self.metrics_every_nodes.hash(&mut hasher);
        self.max_frame_branch_cache.hash(&mut hasher);
        hasher.finish()
    }
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, CheckBytes))]
pub struct SearchBranch {
    pub completion: PayloadVal,
    pub child: Ortho,
    pub optimistic_bound: OrthoScore,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, CheckBytes))]
pub struct SearchFrame {
    pub ortho: Ortho,
    pub optimistic_bound: OrthoScore,
    pub bound_precomputed: bool,
    pub prepared: bool,
    pub next_branch_idx: usize,
    pub branches: Vec<SearchBranch>,
}

impl SearchFrame {
    fn new(ortho: Ortho) -> Self {
        let optimistic_bound = ortho.score();
        Self {
            ortho,
            optimistic_bound,
            bound_precomputed: false,
            prepared: false,
            next_branch_idx: 0,
            branches: Vec::new(),
        }
    }

    fn new_with_precomputed_bound(ortho: Ortho, optimistic_bound: OrthoScore) -> Self {
        Self {
            ortho,
            optimistic_bound,
            bound_precomputed: true,
            prepared: false,
            next_branch_idx: 0,
            branches: Vec::new(),
        }
    }
}

fn verify_bound_reuse_enabled() -> bool {
    static VERIFY_BOUND_REUSE: OnceLock<bool> = OnceLock::new();
    *VERIFY_BOUND_REUSE.get_or_init(|| {
        std::env::var("FOLD_VERIFY_BOUND_REUSE")
            .ok()
            .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false)
    })
}

pub fn bound_reuse_enabled() -> bool {
    true
}

pub fn bound_reuse_shadow_verify_enabled() -> bool {
    verify_bound_reuse_enabled()
}

pub fn parallel_child_bounds_enabled() -> bool {
    static PARALLEL_CHILD_BOUNDS: OnceLock<bool> = OnceLock::new();
    *PARALLEL_CHILD_BOUNDS.get_or_init(|| {
        std::env::var("FOLD_PARALLEL_CHILD_BOUNDS")
            .ok()
            .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false)
    })
}

pub fn parallel_child_bounds_min_branches() -> usize {
    static MIN_BRANCHES: OnceLock<usize> = OnceLock::new();
    *MIN_BRANCHES.get_or_init(|| {
        std::env::var("FOLD_PARALLEL_CHILD_BOUNDS_MIN_BRANCHES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(32)
    })
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, CheckBytes))]
pub struct DfsRunner {
    stack: Vec<SearchFrame>,
    incumbent: Ortho,
    nodes_expanded: u64,
    nodes_pruned: u64,
    completions_pruned: u64,
    seen_by_depth: Vec<u64>,
    descended_by_depth: Vec<u64>,
    pruned_by_depth: Vec<u64>,
    max_depth: usize,
    started_unix: u64,
    last_improvement_unix: u64,
    last_improvement_depth: usize,
    finished: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepEvent {
    pub finished: bool,
    pub incumbent_improved: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchOrdering {
    BestFirst,
    Insertion,
    WorstFirst,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchToggles {
    pub node_pruning: bool,
    pub completion_pruning: bool,
    pub branch_ordering: BranchOrdering,
    pub compute_bounds: bool,
}

impl Default for SearchToggles {
    fn default() -> Self {
        Self {
            node_pruning: true,
            completion_pruning: true,
            branch_ordering: BranchOrdering::BestFirst,
            compute_bounds: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchProfile {
    pub total_step_ns: u128,
    pub existing_bound_ns: u128,
    pub intersect_ns: u128,
    pub child_generation_ns: u128,
    pub reorder_ns: u128,
    pub node_prune_ns: u128,
    pub completion_prune_ns: u128,
    pub completion_bound_ns: u128,
}

#[derive(Clone, Debug)]
pub struct SearchSnapshot {
    pub nodes_expanded: u64,
    pub nodes_pruned: u64,
    pub completions_pruned: u64,
    pub current_depth: usize,
    pub max_depth: usize,
    pub current_bound: Option<OrthoScore>,
    pub open_siblings_total: u64,
    pub open_siblings_by_depth: Vec<u64>,
    pub seen_by_depth: Vec<u64>,
    pub descended_by_depth: Vec<u64>,
    pub pruned_by_depth: Vec<u64>,
    pub path_progress_by_depth: Vec<(usize, usize)>,
    pub frontier_max_bound: Option<OrthoScore>,
    pub started_unix: u64,
    pub last_improvement_unix: u64,
    pub last_improvement_depth: usize,
}

#[derive(Clone, Debug)]
pub struct RunnerSnapshot {
    pub nodes_expanded: u64,
    pub nodes_pruned: u64,
    pub completions_pruned: u64,
    pub current_depth: usize,
    pub max_depth: usize,
    pub current_bound: Option<OrthoScore>,
    pub open_siblings_total: u64,
    pub open_siblings_by_depth: Vec<u64>,
    pub seen_by_depth: Vec<u64>,
    pub descended_by_depth: Vec<u64>,
    pub pruned_by_depth: Vec<u64>,
    pub path_progress_by_depth: Vec<(usize, usize)>,
    pub frontier_max_bound: Option<OrthoScore>,
    pub incumbent: Ortho,
    pub finished: bool,
    pub started_unix: u64,
    pub last_improvement_unix: u64,
    pub last_improvement_depth: usize,
}

impl DfsRunner {
    pub fn new() -> Self {
        let root = Ortho::new();
        let started_unix = now_unix();
        Self {
            stack: vec![SearchFrame::new(root.clone())],
            incumbent: root,
            nodes_expanded: 0,
            nodes_pruned: 0,
            completions_pruned: 0,
            seen_by_depth: Vec::new(),
            descended_by_depth: Vec::new(),
            pruned_by_depth: Vec::new(),
            max_depth: 1,
            started_unix,
            last_improvement_unix: started_unix,
            last_improvement_depth: 1,
            finished: false,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, FoldError> {
        rkyv::to_bytes::<_, 4096>(self)
            .map(|buf| buf.to_vec())
            .map_err(|e| FoldError::Serialization(e.to_string()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FoldError> {
        rkyv::from_bytes::<DfsRunner>(bytes).map_err(|e| FoldError::Deserialization(e.to_string()))
    }

    pub fn incumbent(&self) -> &Ortho {
        &self.incumbent
    }

    pub fn incumbent_score(&self) -> OrthoScore {
        self.incumbent.score()
    }

    pub fn nodes_expanded(&self) -> u64 {
        self.nodes_expanded
    }

    pub fn nodes_pruned(&self) -> u64 {
        self.nodes_pruned
    }

    pub fn completions_pruned(&self) -> u64 {
        self.completions_pruned
    }

    pub fn current_depth(&self) -> usize {
        self.stack.len()
    }

    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    pub fn current_bound(&self) -> Option<OrthoScore> {
        self.stack.last().map(|frame| frame.optimistic_bound)
    }

    pub fn started_unix(&self) -> u64 {
        self.started_unix
    }

    pub fn search_snapshot(&self) -> SearchSnapshot {
        let depth_len = self
            .max_depth
            .max(self.stack.len())
            .max(self.seen_by_depth.len())
            .max(self.descended_by_depth.len())
            .max(self.pruned_by_depth.len());
        let mut open_siblings_total = 0u64;
        let mut open_siblings_by_depth = vec![0u64; depth_len];
        let mut path_progress_by_depth = Vec::with_capacity(self.stack.len());
        let mut frontier_max_bound = None;

        for (depth_idx, frame) in self.stack.iter().enumerate() {
            if frame.prepared {
                path_progress_by_depth.push((
                    frame.next_branch_idx.min(frame.branches.len()),
                    frame.branches.len(),
                ));
                let remaining = frame.branches.len().saturating_sub(frame.next_branch_idx);
                open_siblings_total = open_siblings_total.saturating_add(remaining as u64);
                open_siblings_by_depth[depth_idx] = remaining as u64;
                if frame.next_branch_idx < frame.branches.len() {
                    let candidate = frame.branches[frame.next_branch_idx].optimistic_bound;
                    frontier_max_bound = Some(match frontier_max_bound {
                        Some(existing) if existing >= candidate => existing,
                        _ => candidate,
                    });
                }
            } else {
                path_progress_by_depth.push((0, 0));
            }
        }

        SearchSnapshot {
            nodes_expanded: self.nodes_expanded,
            nodes_pruned: self.nodes_pruned,
            completions_pruned: self.completions_pruned,
            current_depth: self.stack.len(),
            max_depth: self.max_depth,
            current_bound: self.stack.last().map(|frame| frame.optimistic_bound),
            open_siblings_total,
            open_siblings_by_depth,
            seen_by_depth: self.seen_by_depth.clone(),
            descended_by_depth: self.descended_by_depth.clone(),
            pruned_by_depth: self.pruned_by_depth.clone(),
            path_progress_by_depth,
            frontier_max_bound,
            started_unix: self.started_unix,
            last_improvement_unix: self.last_improvement_unix,
            last_improvement_depth: self.last_improvement_depth,
        }
    }

    pub fn snapshot(&self) -> RunnerSnapshot {
        let search = self.search_snapshot();

        RunnerSnapshot {
            nodes_expanded: search.nodes_expanded,
            nodes_pruned: search.nodes_pruned,
            completions_pruned: search.completions_pruned,
            current_depth: search.current_depth,
            max_depth: search.max_depth,
            current_bound: search.current_bound,
            open_siblings_total: search.open_siblings_total,
            open_siblings_by_depth: search.open_siblings_by_depth,
            seen_by_depth: search.seen_by_depth,
            descended_by_depth: search.descended_by_depth,
            pruned_by_depth: search.pruned_by_depth,
            path_progress_by_depth: search.path_progress_by_depth,
            frontier_max_bound: search.frontier_max_bound,
            incumbent: self.incumbent.clone(),
            finished: self.finished,
            started_unix: search.started_unix,
            last_improvement_unix: search.last_improvement_unix,
            last_improvement_depth: search.last_improvement_depth,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn step(&mut self, interner: &Interner) -> Result<StepEvent, FoldError> {
        self.step_with_toggles(interner, &SearchToggles::default())
    }

    pub fn step_with_toggles(
        &mut self,
        interner: &Interner,
        toggles: &SearchToggles,
    ) -> Result<StepEvent, FoldError> {
        self.step_with_toggles_profiled(interner, toggles, None)
    }

    pub fn step_with_toggles_and_profile(
        &mut self,
        interner: &Interner,
        toggles: &SearchToggles,
        profile: &mut SearchProfile,
    ) -> Result<StepEvent, FoldError> {
        self.step_with_toggles_profiled(interner, toggles, Some(profile))
    }

    fn step_with_toggles_profiled(
        &mut self,
        interner: &Interner,
        toggles: &SearchToggles,
        mut profile: Option<&mut SearchProfile>,
    ) -> Result<StepEvent, FoldError> {
        let profiling = profile.is_some();
        macro_rules! profile_start {
            () => {
                profiling.then(Instant::now)
            };
        }
        macro_rules! profile_end {
            ($start:expr, $field:ident) => {
                if let (Some(p), Some(start)) = (profile.as_deref_mut(), $start) {
                    p.$field = p.$field.saturating_add(start.elapsed().as_nanos());
                }
            };
        }

        let step_start = profile_start!();
        if self.finished {
            let event = StepEvent {
                finished: true,
                incumbent_improved: false,
            };
            profile_end!(step_start, total_step_ns);
            return Ok(event);
        }

        let mut incumbent_improved = false;
        let result: Result<StepEvent, FoldError> = STEP_SCRATCH.with(|scratch_cell| {
            let mut scratch = scratch_cell.borrow_mut();
            if scratch.completion_bits.len() < interner.vocab_size() {
                scratch.completion_bits.grow(interner.vocab_size());
            }
            let StepScratch {
                completion_ctx,
                frame_ctx,
                completion_bits,
            } = &mut *scratch;

            loop {
                let mut incumbent_score = self.incumbent_score();
                let current_depth = self.stack.len();
                let Some(frame) = self.stack.last_mut() else {
                    self.finished = true;
                    return Ok(StepEvent {
                        finished: true,
                        incumbent_improved,
                    });
                };

                if !frame.prepared {
                    frame_ctx.reset(&frame.ortho);
                    if toggles.compute_bounds {
                        if !frame.bound_precomputed {
                            let existing_bound_start = profile_start!();
                            frame.optimistic_bound =
                                existing_ortho_upper_bound_ctx(frame_ctx, interner);
                            profile_end!(existing_bound_start, existing_bound_ns);
                        } else if verify_bound_reuse_enabled() {
                            let existing_bound_start = profile_start!();
                            let recomputed = existing_ortho_upper_bound_ctx(frame_ctx, interner);
                            profile_end!(existing_bound_start, existing_bound_ns);
                            if frame.optimistic_bound < recomputed {
                                frame.optimistic_bound = recomputed;
                            }
                        }
                    } else {
                        frame.optimistic_bound = frame.ortho.score();
                    }
                    frame.bound_precomputed = false;
                    let node_prune_start = profile_start!();
                    let prune_root = toggles.node_pruning && frame.optimistic_bound <= incumbent_score;
                    profile_end!(node_prune_start, node_prune_ns);
                    if prune_root {
                        self.nodes_pruned = self.nodes_pruned.saturating_add(1);
                        Self::bump_depth_counter(&mut self.pruned_by_depth, current_depth);
                        self.stack.pop();
                        continue;
                    }

                    completion_ctx.reset(&frame.ortho);
                    completion_bits.clear();
                    let intersect_start = profile_start!();
                    interner.intersect_into(
                        completion_ctx.required_usize(),
                        completion_ctx.forbidden_usize(),
                        completion_bits,
                    );
                    profile_end!(intersect_start, intersect_ns);

                    frame.branches.clear();
                    frame.next_branch_idx = 0;

                    for completion in completion_bits.ones() {
                        let completion_bound = if toggles.compute_bounds {
                            let completion_bound_start = profile_start!();
                            let Some(bound) = completion_upper_bound_ctx(completion_ctx, completion, interner) else {
                                profile_end!(completion_bound_start, completion_bound_ns);
                                self.completions_pruned = self.completions_pruned.saturating_add(1);
                                continue;
                            };
                            profile_end!(completion_bound_start, completion_bound_ns);
                            bound
                        } else {
                            OrthoScore::optimistic_bound(1, 1)
                        };

                        let completion_prune_start = profile_start!();
                        let prune_completion =
                            toggles.completion_pruning && toggles.compute_bounds && completion_bound <= incumbent_score;
                        profile_end!(completion_prune_start, completion_prune_ns);
                        if prune_completion {
                            self.completions_pruned = self.completions_pruned.saturating_add(1);
                            continue;
                        }

                        let completion_val =
                            PayloadVal::try_from(completion).expect("completion overflowed u32");
                        let child_generation_start = profile_start!();
                        for child in frame.ortho.add(completion_val) {
                            self.nodes_expanded = self.nodes_expanded.saturating_add(1);

                            let child_score = child.score();
                            if child_score > incumbent_score {
                                self.incumbent = child.clone();
                                incumbent_score = child_score;
                                incumbent_improved = true;
                                self.last_improvement_unix = now_unix();
                                self.last_improvement_depth = current_depth.saturating_add(1);
                            }

                            frame.branches.push(SearchBranch {
                                completion: completion_val,
                                child,
                                optimistic_bound: child_score,
                            });
                        }
                        profile_end!(child_generation_start, child_generation_ns);
                    }

                    if toggles.compute_bounds && !frame.branches.is_empty() {
                        let existing_bound_start = profile_start!();
                        let should_parallelize = parallel_child_bounds_enabled()
                            && frame.branches.len() >= parallel_child_bounds_min_branches()
                            && std::thread::available_parallelism()
                                .map(|n| n.get() > 1)
                                .unwrap_or(false);

                        if should_parallelize {
                            frame.branches.par_iter_mut().for_each(|branch| {
                                let mut local_ctx = CompletionContext::from_ortho(&branch.child);
                                branch.optimistic_bound =
                                    existing_ortho_upper_bound_ctx(&mut local_ctx, interner);
                            });
                        } else {
                            for branch in &mut frame.branches {
                                frame_ctx.reset(&branch.child);
                                branch.optimistic_bound = existing_ortho_upper_bound_ctx(frame_ctx, interner);
                            }
                        }

                        profile_end!(existing_bound_start, existing_bound_ns);
                    }

                    match toggles.branch_ordering {
                    BranchOrdering::BestFirst => {
                        if toggles.compute_bounds {
                            let reorder_start = profile_start!();
                            frame.branches.sort_by(|a, b| {
                                b.optimistic_bound
                                    .cmp(&a.optimistic_bound)
                                    .then_with(|| b.child.score().cmp(&a.child.score()))
                                    .then_with(|| a.completion.cmp(&b.completion))
                                    .then_with(|| a.child.id().cmp(&b.child.id()))
                            });
                            profile_end!(reorder_start, reorder_ns);
                        }
                    }
                    BranchOrdering::Insertion => {}
                    BranchOrdering::WorstFirst => {
                        if toggles.compute_bounds {
                            let reorder_start = profile_start!();
                            frame.branches.sort_by(|a, b| {
                                a.optimistic_bound
                                    .cmp(&b.optimistic_bound)
                                    .then_with(|| a.child.score().cmp(&b.child.score()))
                                    .then_with(|| a.completion.cmp(&b.completion))
                                    .then_with(|| a.child.id().cmp(&b.child.id()))
                            });
                            profile_end!(reorder_start, reorder_ns);
                        }
                    }
                }

                    frame.prepared = true;
                    Self::add_depth_counter(
                        &mut self.seen_by_depth,
                        current_depth,
                        frame.branches.len() as u64,
                    );
                    if frame.branches.is_empty() {
                        self.stack.pop();
                        continue;
                    }
                }

                while frame.next_branch_idx < frame.branches.len() {
                    let branch = frame.branches[frame.next_branch_idx].clone();
                    frame.next_branch_idx += 1;
                    let node_prune_start = profile_start!();
                    let prune_branch = toggles.node_pruning && branch.optimistic_bound <= incumbent_score;
                    profile_end!(node_prune_start, node_prune_ns);
                    if prune_branch {
                        self.nodes_pruned = self.nodes_pruned.saturating_add(1);
                        Self::bump_depth_counter(&mut self.pruned_by_depth, current_depth);
                        continue;
                    }

                    Self::bump_depth_counter(&mut self.descended_by_depth, current_depth);
                    self.stack.push(SearchFrame::new_with_precomputed_bound(
                        branch.child,
                        branch.optimistic_bound,
                    ));
                    self.max_depth = self.max_depth.max(self.stack.len());
                    return Ok(StepEvent {
                        finished: false,
                        incumbent_improved,
                    });
                }

                self.stack.pop();
            }
        });
        let event = result?;
        profile_end!(step_start, total_step_ns);
        Ok(event)
    }

    fn bump_depth_counter(counter: &mut Vec<u64>, depth: usize) {
        Self::add_depth_counter(counter, depth, 1);
    }

    fn add_depth_counter(counter: &mut Vec<u64>, depth: usize, amount: u64) {
        if depth == 0 {
            return;
        }
        if counter.len() < depth {
            counter.resize(depth, 0);
        }
        counter[depth - 1] = counter[depth - 1].saturating_add(amount);
    }
}

pub fn compare_ortho_for_best(a: &Ortho, b: &Ortho) -> Ordering {
    a.score().cmp(&b.score()).then_with(|| b.id().cmp(&a.id()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
