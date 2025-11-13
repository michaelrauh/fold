use crate::spatial;
use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};
use std::fmt;
use bincode::Encode;
use bincode::Decode;

#[derive(PartialEq, Debug, Clone, Encode, Decode)]
pub struct Ortho {
    dims: Vec<usize>,
    payload: Vec<Option<usize>>,
}

impl Ortho {
    fn compute_id(dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
        // Compute ID based on canonical state (dims + payload)
        // This ensures path-independent IDs - orthos with same final state get same ID
        let mut hasher = FxHasher::default();
        dims.hash(&mut hasher);
        payload.hash(&mut hasher);
        (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
    }
    
    pub fn new(_version: usize) -> Self {
        let dims = vec![2,2];
        let payload = vec![None; 4];
        Ortho { dims, payload }
    }
    
    pub fn id(&self) -> usize {
        Self::compute_id(&self.dims, &self.payload)
    }
    
    pub fn get_current_position(&self) -> usize { self.payload.iter().position(|x| x.is_none()).unwrap_or(self.payload.len()) }
    pub fn add(&self, value: usize, _version: usize) -> Vec<Self> {
        let insertion_index = self.get_current_position();
        let total_empty = self.payload.iter().filter(|x| x.is_none()).count();
        
        if total_empty == 1 {
            if spatial::is_base(&self.dims) {
                return Self::expand(
                    self,
                    spatial::expand_up(&self.dims, self.get_insert_position(value)),
                    value,
                );
            } else {
                return Self::expand(self, spatial::expand_over(&self.dims), value);
            }
        }
        if insertion_index == 2 && self.dims.as_slice() == [2, 2] {
            let mut new_payload: Vec<Option<usize>> = self.payload.clone();
            new_payload[insertion_index] = Some(value);
            if let (Some(second), Some(third)) = (new_payload[1], new_payload[2]) {
                if second > third { new_payload[1] = Some(third); new_payload[2] = Some(second); }
            }
            return vec![Ortho { dims: self.dims.clone(), payload: new_payload }];
        }
        let len = self.payload.len();
        let mut new_payload: Vec<Option<usize>> = Vec::with_capacity(len);
        unsafe { new_payload.set_len(len); std::ptr::copy_nonoverlapping(self.payload.as_ptr(), new_payload.as_mut_ptr(), len); }
        if insertion_index < new_payload.len() { new_payload[insertion_index] = Some(value); }
        vec![Ortho { dims: self.dims.clone(), payload: new_payload }]
    }
    fn expand(
        ortho: &Ortho,
        expansions: Vec<(Vec<usize>, usize, Vec<usize>)>,
        value: usize,
    ) -> Vec<Ortho> {
        // Find insert position once
        let insert_pos = ortho.payload.iter().position(|x| x.is_none()).unwrap();
        
        let mut out = Vec::with_capacity(expansions.len());
        for (new_dims_vec, new_capacity, reorg) in expansions.into_iter() {
            let mut new_payload = vec![None; new_capacity];
            // Directly reorganize old payload, inserting value at the right position
            for (i, &pos) in reorg.iter().enumerate() {
                if i == insert_pos {
                    new_payload[pos] = Some(value);
                } else {
                    new_payload[pos] = ortho.payload.get(i).cloned().flatten();
                }
            }
            out.push(Ortho { dims: new_dims_vec, payload: new_payload });
        }
        out
    }
    fn get_insert_position(&self, to_add: usize) -> usize {
        let axis_positions = spatial::get_axis_positions(&self.dims);
        let mut idx = 0;
        for &pos in axis_positions.iter() {
            if let Some(&axis) = self.payload.get(pos).and_then(|x| x.as_ref()) {
                if to_add < axis { return idx; }
                idx += 1;
            }
        }
        idx
    }
    pub fn get_requirements(&self) -> (Vec<usize>, Vec<Vec<usize>>) {
        let pos = self.get_current_position();
        let used_tokens = self.payload.iter().filter_map(|x| *x).collect();
        let axis_positions = spatial::get_axis_positions(&self.dims);
        let mut required_supersets = vec![];
        for axis_pos in axis_positions.into_iter().skip(pos) {
            let supertoken = self.payload.get(axis_pos).and_then(|x| x.as_ref());
            if let Some(&token) = supertoken { required_supersets.push(vec![token]); }
        }
        (used_tokens, required_supersets)
    }
    pub fn dims(&self) -> &Vec<usize> { &self.dims }
    pub fn payload(&self) -> &Vec<Option<usize>> { &self.payload }
}

impl fmt::Display for Ortho {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ortho {{ id: {}, dims: {:?}, payload: {:?} }}", self.id(), self.dims, self.payload)
    }
}
