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
use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use std::cmp::Ordering;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn saturating_pow_usize(base: usize, exp: usize) -> usize {
    (base as u128)
        .checked_pow(exp as u32)
        .unwrap_or(u128::MAX)
        .min(usize::MAX as u128) as usize
}

#[derive(Default)]
pub(crate) struct StepScratch {
    frame_ctx: CompletionContext,
    child_ctx: CompletionContext,
    completion_bits: FixedBitSet,
    child_scratch: Vec<Ortho>,
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
    pub min_insert_axis: usize,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, CheckBytes))]
pub struct SearchFrame {
    pub ortho: Ortho,
    pub optimistic_bound: OrthoScore,
    pub bound_precomputed: bool,
    pub prepared: bool,
    pub initial_branches_len: usize,
    pub branches: Vec<SearchBranch>,
    pub min_insert_axis: usize,
}

impl SearchFrame {
    fn new(ortho: Ortho) -> Self {
        let optimistic_bound = ortho.score();
        Self {
            ortho,
            optimistic_bound,
            bound_precomputed: false,
            prepared: false,
            initial_branches_len: 0,
            branches: Vec::new(),
            min_insert_axis: 0,
        }
    }

    pub fn new_with_bound_and_min_axis(
        ortho: Ortho,
        optimistic_bound: OrthoScore,
        min_insert_axis: usize,
    ) -> Self {
        Self {
            ortho,
            optimistic_bound,
            bound_precomputed: true,
            prepared: false,
            initial_branches_len: 0,
            branches: Vec::new(),
            min_insert_axis,
        }
    }
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
    score_floor: OrthoScore,
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
            branch_ordering: BranchOrdering::Insertion,
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
    pub ctx_reset_ns: u128,
    pub k_bound_ns: u128,
    pub depth_counter_ns: u128,
    pub completion_iter_ns: u128,
    pub completion_bound_attempts: u64,
    pub completion_bound_pruned: u64,
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

fn record_completion_bound_result(pruned: bool, profile: Option<&mut SearchProfile>) {
    if let Some(profile) = profile {
        profile.completion_bound_attempts = profile.completion_bound_attempts.saturating_add(1);
        if pruned {
            profile.completion_bound_pruned = profile.completion_bound_pruned.saturating_add(1);
        }
    }
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
            score_floor: OrthoScore::optimistic_bound(8, 27),
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

    pub fn from_stack(stack: Vec<SearchFrame>, incumbent: Ortho) -> Self {
        let started_unix = now_unix();
        Self {
            max_depth: stack.len().max(1),
            stack,
            incumbent,
            nodes_expanded: 0,
            nodes_pruned: 0,
            completions_pruned: 0,
            seen_by_depth: Vec::new(),
            descended_by_depth: Vec::new(),
            pruned_by_depth: Vec::new(),
            started_unix,
            last_improvement_unix: started_unix,
            last_improvement_depth: 1,
            finished: false,
            score_floor: OrthoScore::optimistic_bound(8, 27),
        }
    }

    pub fn frontier_shards(
        interner: &Interner,
        shard_depth: usize,
        toggles: &SearchToggles,
    ) -> Result<(Vec<Vec<SearchFrame>>, Vec<Vec<u64>>, Vec<u64>), FoldError> {
        let target_depth = shard_depth.max(1);
        let mut runner = DfsRunner::new();
        let mut shards: Vec<(Vec<SearchFrame>, Vec<u64>)> = Vec::new();
        while !runner.is_finished() {
            while !runner.is_finished() && runner.stack.len() < target_depth {
                runner.step_with_toggles(interner, toggles)?;
            }
            if runner.is_finished() {
                break;
            }
            let ancestors: Vec<u64> = runner.stack.iter().map(|f| f.ortho.id()).collect();
            let mut shard_stack = runner.stack.clone();
            let leaf = shard_stack.len().saturating_sub(1);
            for frame in &mut shard_stack[..leaf] {
                frame.branches = Vec::new();
                frame.initial_branches_len = 0;
            }
            shards.push((shard_stack, ancestors));
            runner.stack.pop();
        }
        if shards.is_empty() {
            shards.push((vec![SearchFrame::new(Ortho::new())], Vec::new()));
        }
        let snap = runner.search_snapshot();
        let (shard_stacks, shard_ancestors) = shards.into_iter().unzip();
        Ok((shard_stacks, shard_ancestors, snap.seen_by_depth))
    }

    pub fn incumbent(&self) -> &Ortho {
        &self.incumbent
    }

    pub fn actual_incumbent_score(&self) -> OrthoScore {
        self.incumbent.score()
    }

    pub fn incumbent_score(&self) -> OrthoScore {
        self.incumbent.score().max(self.score_floor)
    }

    pub fn score_floor(&self) -> OrthoScore {
        self.score_floor
    }

    pub fn top_frame_bound(&self) -> Option<OrthoScore> {
        self.stack.last().map(|f| f.optimistic_bound)
    }

    pub fn import_incumbent_if_better(&mut self, incumbent: &Ortho) -> bool {
        if incumbent.score() > self.incumbent.score() {
            self.incumbent = incumbent.clone();
            true
        } else {
            false
        }
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
                let processed = frame
                    .initial_branches_len
                    .saturating_sub(frame.branches.len());
                path_progress_by_depth.push((processed, frame.initial_branches_len));
                let remaining = frame.branches.len();
                open_siblings_total = open_siblings_total.saturating_add(remaining as u64);
                open_siblings_by_depth[depth_idx] = remaining as u64;
                if let Some(next_branch) = frame.branches.last() {
                    let candidate = next_branch.optimistic_bound;
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
        let mut scratch = StepScratch::default();
        self.step_with_toggles_profiled(interner, toggles, &mut scratch, None)
    }

    pub(crate) fn step_with_toggles_and_scratch(
        &mut self,
        interner: &Interner,
        toggles: &SearchToggles,
        scratch: &mut StepScratch,
    ) -> Result<StepEvent, FoldError> {
        self.step_with_toggles_profiled(interner, toggles, scratch, None)
    }

    pub fn step_with_toggles_and_profile(
        &mut self,
        interner: &Interner,
        toggles: &SearchToggles,
        profile: &mut SearchProfile,
    ) -> Result<StepEvent, FoldError> {
        let mut scratch = StepScratch::default();
        self.step_with_toggles_profiled(interner, toggles, &mut scratch, Some(profile))
    }

    fn step_with_toggles_profiled(
        &mut self,
        interner: &Interner,
        toggles: &SearchToggles,
        scratch: &mut StepScratch,
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
        if scratch.completion_bits.len() < interner.vocab_size() {
            scratch.completion_bits.grow(interner.vocab_size());
        }
        let StepScratch {
            frame_ctx,
            child_ctx,
            completion_bits,
            child_scratch,
        } = scratch;

        let result: Result<StepEvent, FoldError> = 'step: loop {
            let mut incumbent_score = self.incumbent_score();
            let mut actual_incumbent_score = self.incumbent.score();
            let current_depth = self.stack.len();
            let Some(frame) = self.stack.last_mut() else {
                self.finished = true;
                break Ok(StepEvent {
                    finished: true,
                    incumbent_improved,
                });
            };

            if !frame.prepared {
                let prune_completions = toggles.compute_bounds && toggles.completion_pruning;
                let ctx_reset_start = profile_start!();
                if prune_completions {
                    frame_ctx.reset_for_completion_bounds(&frame.ortho);
                } else {
                    frame_ctx.reset_for_node(&frame.ortho);
                }
                profile_end!(ctx_reset_start, ctx_reset_ns);
                if toggles.compute_bounds {
                    if !frame.bound_precomputed {
                        let existing_bound_start = profile_start!();
                        frame.optimistic_bound =
                            existing_ortho_upper_bound_ctx(frame_ctx, interner);
                        profile_end!(existing_bound_start, existing_bound_ns);
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
                    self.stack.pop();
                    continue;
                }

                let ctx_reset_start = profile_start!();
                completion_bits.clear();
                profile_end!(ctx_reset_start, ctx_reset_ns);
                let intersect_start = profile_start!();
                let k = interner.intersect_into_count(
                    frame_ctx.required_usize(),
                    frame_ctx.forbidden_usize(),
                    completion_bits,
                );
                profile_end!(intersect_start, intersect_ns);

                // Tighten bound using intersection count: if only k completions exist,
                // then each axis can hold at most k values, so vol <= (k-1)^dim_count.
                let intersection_prune = if toggles.compute_bounds && toggles.node_pruning {
                    let k_bound_start = profile_start!();
                    let dim_count = frame_ctx.dim_count();
                    let k_vol = saturating_pow_usize(k.saturating_sub(1), dim_count)
                        .max(frame_ctx.base_volume());
                    let k_full = saturating_pow_usize(k, dim_count).max(frame_ctx.base_fullness());
                    let k_bound = OrthoScore::optimistic_bound(k_vol, k_full);
                    if k_bound < frame.optimistic_bound {
                        frame.optimistic_bound = k_bound;
                    }
                    let prune_result = frame.optimistic_bound <= incumbent_score;
                    profile_end!(k_bound_start, k_bound_ns);
                    prune_result
                } else {
                    false
                };

                frame.branches.clear();

                if intersection_prune {
                    self.nodes_pruned = self.nodes_pruned.saturating_add(1);
                    self.stack.pop();
                    continue;
                }

                for completion in completion_bits.ones() {
                    let completion_bound = if prune_completions {
                        let completion_bound_start = profile_start!();
                        let Some(completion_bound) =
                            completion_upper_bound_ctx(frame_ctx, completion, interner)
                        else {
                            profile_end!(completion_bound_start, completion_bound_ns);
                            record_completion_bound_result(true, profile.as_deref_mut());
                            self.completions_pruned = self.completions_pruned.saturating_add(1);
                            continue;
                        };
                        profile_end!(completion_bound_start, completion_bound_ns);
                        let completion_prune_start = profile_start!();
                        let prune_completion = completion_bound <= incumbent_score;
                        profile_end!(completion_prune_start, completion_prune_ns);
                        record_completion_bound_result(prune_completion, profile.as_deref_mut());
                        if prune_completion {
                            self.completions_pruned = self.completions_pruned.saturating_add(1);
                            continue;
                        }
                        Some(completion_bound)
                    } else {
                        None
                    };

                    let completion_val =
                        PayloadVal::try_from(completion).expect("completion overflowed u32");

                    let completion_iter_start = profile_start!();
                    let maybe_axis = frame.ortho.expanding_insert_axis(completion_val);
                    if let Some(axis) = maybe_axis {
                        if axis < frame.min_insert_axis {
                            profile_end!(completion_iter_start, completion_iter_ns);
                            continue;
                        }
                    }
                    let child_min_insert_axis = maybe_axis.unwrap_or(frame.min_insert_axis);
                    profile_end!(completion_iter_start, completion_iter_ns);

                    let child_generation_start = profile_start!();
                    child_scratch.clear();
                    frame.ortho.add_into(completion_val, child_scratch);
                    for child in child_scratch.drain(..) {
                        self.nodes_expanded = self.nodes_expanded.saturating_add(1);

                        let child_score = child.score();
                        if child_score > actual_incumbent_score {
                            self.incumbent = child.clone();
                            actual_incumbent_score = child_score;
                            incumbent_score = child_score.max(self.score_floor);
                            incumbent_improved = true;
                            self.last_improvement_unix = now_unix();
                            self.last_improvement_depth = current_depth.saturating_add(1);
                        }

                        let optimistic_bound = if let Some(completion_bound) = completion_bound
                            .filter(|_| child.dims() == frame.ortho.dims())
                            .filter(|_| child.up_axis() == frame.ortho.up_axis())
                        {
                            completion_bound.max(child_score)
                        } else if toggles.compute_bounds && prune_completions {
                            let existing_bound_start = profile_start!();
                            child_ctx.reset_for_node(&child);
                            let bound = existing_ortho_upper_bound_ctx(child_ctx, interner);
                            profile_end!(existing_bound_start, existing_bound_ns);
                            bound
                        } else {
                            child_score
                        };

                        frame.branches.push(SearchBranch {
                            completion: completion_val,
                            child,
                            optimistic_bound,
                            min_insert_axis: child_min_insert_axis,
                        });
                    }
                    profile_end!(child_generation_start, child_generation_ns);
                }

                if toggles.compute_bounds && !prune_completions && !frame.branches.is_empty() {
                    let existing_bound_start = profile_start!();
                    for branch in &mut frame.branches {
                        frame_ctx.reset_for_node(&branch.child);
                        branch.optimistic_bound =
                            existing_ortho_upper_bound_ctx(frame_ctx, interner);
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

                frame.branches.reverse();
                frame.initial_branches_len = frame.branches.len();
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

            while let Some(branch) = frame.branches.pop() {
                // Per-dims generational dedup: skip orthos already expanded this generation.
                let node_prune_start = profile_start!();
                let prune_branch =
                    toggles.node_pruning && branch.optimistic_bound <= incumbent_score;
                profile_end!(node_prune_start, node_prune_ns);
                if prune_branch {
                    self.nodes_pruned = self.nodes_pruned.saturating_add(1);
                    Self::bump_depth_counter(&mut self.pruned_by_depth, current_depth);
                    continue;
                }

                Self::bump_depth_counter(&mut self.descended_by_depth, current_depth);
                let depth_counter_start = profile_start!();
                for v in self.seen_by_depth.iter_mut().skip(current_depth) {
                    *v = 0;
                }
                for v in self.descended_by_depth.iter_mut().skip(current_depth) {
                    *v = 0;
                }
                for v in self.pruned_by_depth.iter_mut().skip(current_depth) {
                    *v = 0;
                }
                self.stack.push(SearchFrame::new_with_bound_and_min_axis(
                    branch.child,
                    branch.optimistic_bound,
                    branch.min_insert_axis,
                ));
                profile_end!(depth_counter_start, depth_counter_ns);
                self.max_depth = self.max_depth.max(self.stack.len());
                break 'step Ok(StepEvent {
                    finished: false,
                    incumbent_improved,
                });
            }

            self.stack.pop();
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::{EMPTY_CELL, payload_to_usize};

    fn vocab_index(interner: &Interner, word: &str) -> usize {
        interner
            .vocabulary()
            .iter()
            .position(|candidate| candidate == word)
            .expect("word not found in vocab")
    }

    fn runner_after_word(interner: &Interner, word: &str, score_floor: OrthoScore) -> DfsRunner {
        let word_idx = vocab_index(interner, word);
        let ortho = Ortho::new().add(PayloadVal::try_from(word_idx).unwrap())[0].clone();
        let mut runner = DfsRunner::from_stack(vec![SearchFrame::new(ortho)], Ortho::new());
        runner.score_floor = score_floor;
        runner
    }

    #[test]
    fn completion_pruning_toggle_controls_completion_pruned_count() {
        let interner = Interner::from_text("x y. x z z z z");
        let score_floor = OrthoScore::optimistic_bound(2, 4);

        let mut with_pruning = runner_after_word(&interner, "x", score_floor);
        let pruning_toggles = SearchToggles {
            node_pruning: false,
            completion_pruning: true,
            branch_ordering: BranchOrdering::Insertion,
            compute_bounds: true,
        };
        with_pruning
            .step_with_toggles(&interner, &pruning_toggles)
            .unwrap();
        assert!(
            with_pruning.completions_pruned() > 0,
            "score-based completion pruning should skip the shallow x y branch"
        );

        let mut without_pruning = runner_after_word(&interner, "x", score_floor);
        let no_pruning_toggles = SearchToggles {
            completion_pruning: false,
            ..pruning_toggles
        };
        without_pruning
            .step_with_toggles(&interner, &no_pruning_toggles)
            .unwrap();
        assert_eq!(
            without_pruning.completions_pruned(),
            0,
            "disabled completion pruning should not count skipped completions"
        );
    }

    #[test]
    fn disabled_completion_pruning_skips_completion_bound_work() {
        let interner = Interner::from_text("x y. x z z z z");
        let score_floor = OrthoScore::optimistic_bound(2, 4);
        let mut runner = runner_after_word(&interner, "x", score_floor);
        let toggles = SearchToggles {
            node_pruning: false,
            completion_pruning: false,
            branch_ordering: BranchOrdering::Insertion,
            compute_bounds: true,
        };
        let mut profile = SearchProfile::default();

        runner
            .step_with_toggles_and_profile(&interner, &toggles, &mut profile)
            .unwrap();

        assert_eq!(runner.completions_pruned(), 0);
        assert_eq!(
            profile.completion_bound_ns, 0,
            "completion bounds should not be computed when completion pruning is disabled"
        );
    }

    #[test]
    fn root_single_span_pruning_is_tied_to_completion_pruning() {
        let interner = Interner::from_text("a b");
        let mut runner = DfsRunner::new();
        runner.score_floor = OrthoScore::zero();
        let toggles = SearchToggles {
            node_pruning: false,
            completion_pruning: true,
            branch_ordering: BranchOrdering::Insertion,
            compute_bounds: true,
        };

        runner.step_with_toggles(&interner, &toggles).unwrap();

        assert!(
            runner.completions_pruned() > 0,
            "single-span root candidates should still prune when completion pruning is enabled"
        );
        assert!(
            runner
                .incumbent()
                .payload_raw()
                .iter()
                .filter(|&&v| v != EMPTY_CELL)
                .all(|&value| payload_to_usize(value) < interner.vocab_size())
        );
    }
}
