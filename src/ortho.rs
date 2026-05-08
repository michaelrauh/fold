use crate::{FoldError, spatial};
use bytecheck::CheckBytes;
use rkyv::{Archive, Deserialize, Serialize};
use rustc_hash::FxHasher;
use std::cmp::Ordering;
use std::fmt;
use std::hash::Hash;

pub type Dim = u8;
pub type PayloadVal = u32;
pub type OrthoId = u64;

pub const MAX_DIMS: usize = 8;
pub const MAX_PAYLOAD: usize = 64;
pub const EMPTY_CELL: PayloadVal = PayloadVal::MAX;

pub fn dim_to_usize(value: Dim) -> usize {
    usize::try_from(value).expect("dim overflowed usize")
}

pub fn payload_to_usize(value: PayloadVal) -> usize {
    usize::try_from(value).expect("payload value overflowed usize")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, PartialEq, CheckBytes))]
pub struct OrthoScore {
    pub volume: usize,
    pub variance_num: u128,
    pub variance_den: u128,
    pub fullness: usize,
}

impl OrthoScore {
    pub const fn zero() -> Self {
        Self {
            volume: 0,
            variance_num: 0,
            variance_den: 1,
            fullness: 0,
        }
    }

    pub const fn optimistic_bound(volume: usize, fullness: usize) -> Self {
        Self {
            volume,
            variance_num: 0,
            variance_den: 1,
            fullness,
        }
    }

    pub fn variance_cmp(&self, other: &Self) -> Ordering {
        let lhs = self.variance_num.saturating_mul(other.variance_den);
        let rhs = other.variance_num.saturating_mul(self.variance_den);
        lhs.cmp(&rhs)
    }

    pub fn variance_as_f64(&self) -> f64 {
        if self.variance_den == 0 {
            return 0.0;
        }
        self.variance_num as f64 / self.variance_den as f64
    }
}

impl Default for OrthoScore {
    fn default() -> Self {
        Self::zero()
    }
}

impl Ord for OrthoScore {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.volume.cmp(&other.volume) {
            Ordering::Equal => match self.variance_cmp(other) {
                Ordering::Less => Ordering::Greater,
                Ordering::Greater => Ordering::Less,
                Ordering::Equal => self.fullness.cmp(&other.fullness),
            },
            non_eq => non_eq,
        }
    }
}

impl PartialOrd for OrthoScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(PartialEq, Debug, Clone, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, PartialEq, CheckBytes))]
pub struct Ortho {
    dims: [Dim; MAX_DIMS],
    dims_len: u8,
    payload: [PayloadVal; MAX_PAYLOAD],
    payload_cap: u8,
    up_axis: Option<Dim>, // Records the last "up" transform axis (None = last expansion was over or base)
    fill_count: u32,      // Cached number of filled payload cells
    next_empty: u32,      // Cached insertion position (payload_cap when full)
    score: OrthoScore,    // Cached score for hot comparisons
    id: OrthoId,          // Cached hash of dims/payload for fast lookups
}

impl Ortho {
    #[inline]
    fn mix_id_word(mut value: u64) -> u64 {
        value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    #[inline]
    fn payload_cell_id(idx: usize, value: PayloadVal) -> OrthoId {
        Self::mix_id_word(((idx as u64) << 32) ^ u64::from(value)) & 0x7FFF_FFFF_FFFF_FFFF
    }

    fn compute_id(dims: &[Dim], payload: &[PayloadVal], up_axis: Option<Dim>) -> OrthoId {
        use std::hash::Hasher;
        let mut hasher = FxHasher::default();
        dims.hash(&mut hasher);
        hasher.write_usize(payload.len());
        up_axis.hash(&mut hasher);
        let mut id = hasher.finish();
        for (idx, &value) in payload.iter().enumerate() {
            if value != EMPTY_CELL {
                id ^= Self::payload_cell_id(idx, value);
            }
        }
        id & 0x7FFF_FFFF_FFFF_FFFF
    }

    fn dims_to_inline(dims: &[Dim]) -> ([Dim; MAX_DIMS], u8) {
        debug_assert!(
            dims.len() <= MAX_DIMS,
            "dims length {} exceeds MAX_DIMS {}",
            dims.len(),
            MAX_DIMS
        );
        let mut arr = [0u8; MAX_DIMS];
        arr[..dims.len()].copy_from_slice(dims);
        (arr, dims.len() as u8)
    }

    fn opts_to_inline(payload: &[Option<PayloadVal>]) -> ([PayloadVal; MAX_PAYLOAD], u8) {
        debug_assert!(
            payload.len() <= MAX_PAYLOAD,
            "payload length {} exceeds MAX_PAYLOAD {}",
            payload.len(),
            MAX_PAYLOAD
        );
        let mut arr = [EMPTY_CELL; MAX_PAYLOAD];
        for (i, &opt) in payload.iter().enumerate() {
            arr[i] = opt.unwrap_or(EMPTY_CELL);
        }
        (arr, payload.len() as u8)
    }

    fn from_parts(dims: &[Dim], payload: &[Option<PayloadVal>], up_axis: Option<Dim>) -> Self {
        let (dims_arr, dims_len) = Self::dims_to_inline(dims);
        let (payload_arr, payload_cap) = Self::opts_to_inline(payload);
        let id = Self::compute_id(
            &dims_arr[..dims_len as usize],
            &payload_arr[..payload_cap as usize],
            up_axis,
        );
        let (fill_count, next_empty, score) = Self::compute_cached_fields(
            &dims_arr[..dims_len as usize],
            &payload_arr,
            payload_cap as usize,
        );
        Self {
            dims: dims_arr,
            dims_len,
            payload: payload_arr,
            payload_cap,
            up_axis,
            fill_count,
            next_empty,
            score,
            id,
        }
    }

    fn from_parts_raw(
        dims: &[Dim],
        payload_arr: [PayloadVal; MAX_PAYLOAD],
        payload_cap: u8,
        up_axis: Option<Dim>,
    ) -> Self {
        let (dims_arr, dims_len) = Self::dims_to_inline(dims);
        let id = Self::compute_id(
            &dims_arr[..dims_len as usize],
            &payload_arr[..payload_cap as usize],
            up_axis,
        );
        let (fill_count, next_empty, score) = Self::compute_cached_fields(
            &dims_arr[..dims_len as usize],
            &payload_arr,
            payload_cap as usize,
        );
        Self {
            dims: dims_arr,
            dims_len,
            payload: payload_arr,
            payload_cap,
            up_axis,
            fill_count,
            next_empty,
            score,
            id,
        }
    }

    #[inline]
    fn next_empty_after(payload: &[PayloadVal; MAX_PAYLOAD], cap: usize, start: usize) -> usize {
        for idx in start..cap {
            if payload[idx] == EMPTY_CELL {
                return idx;
            }
        }
        cap
    }

    #[inline]
    fn in_fill_child_from_payload_raw(
        &self,
        payload_arr: [PayloadVal; MAX_PAYLOAD],
        id: OrthoId,
    ) -> Self {
        let fill_count = self
            .fill_count
            .checked_add(1)
            .expect("fill count overflowed cached u32 field");
        let next_empty = Self::next_empty_after(
            &payload_arr,
            self.payload_cap as usize,
            self.next_empty as usize + 1,
        );
        let next_empty =
            u32::try_from(next_empty).expect("next empty position overflowed cached u32 field");
        let mut score = self.score;
        score.fullness = fill_count as usize;
        Self {
            dims: self.dims,
            dims_len: self.dims_len,
            payload: payload_arr,
            payload_cap: self.payload_cap,
            up_axis: self.up_axis,
            fill_count,
            next_empty,
            score,
            id,
        }
    }

    fn compute_score_components(dims: &[Dim], fullness: usize) -> OrthoScore {
        let volume = dims
            .iter()
            .map(|x| usize::from(*x).saturating_sub(1))
            .product::<usize>();
        let dim_count = dims.len() as u128;
        let dim_sum = dims.iter().map(|&d| u128::from(d)).sum::<u128>();
        let dim_sum_sq = dims
            .iter()
            .map(|&d| {
                let value = u128::from(d);
                value.saturating_mul(value)
            })
            .sum::<u128>();
        let variance_num = dim_count
            .saturating_mul(dim_sum_sq)
            .saturating_sub(dim_sum.saturating_mul(dim_sum));
        let variance_den = dim_count.saturating_mul(dim_count).max(1);
        OrthoScore {
            volume,
            variance_num,
            variance_den,
            fullness,
        }
    }

    fn compute_cached_fields(
        dims: &[Dim],
        payload: &[PayloadVal; MAX_PAYLOAD],
        cap: usize,
    ) -> (u32, u32, OrthoScore) {
        let mut fill_count = 0usize;
        let mut next_empty = cap;
        for idx in 0..cap {
            if payload[idx] != EMPTY_CELL {
                fill_count += 1;
            } else if next_empty == cap {
                next_empty = idx;
            }
        }
        let fill_count_u32 =
            u32::try_from(fill_count).expect("fill count overflowed cached u32 field");
        let next_empty_u32 =
            u32::try_from(next_empty).expect("next empty position overflowed cached u32 field");
        let score = Self::compute_score_components(dims, fill_count);
        (fill_count_u32, next_empty_u32, score)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn from_test_parts(
        dims: Vec<Dim>,
        payload: Vec<Option<PayloadVal>>,
        up_axis: Option<Dim>,
    ) -> Self {
        Self::from_parts(&dims, &payload, up_axis)
    }

    pub fn new() -> Self {
        let dims: &[Dim] = &[2, 2];
        let payload = &[None; 4];
        let up_axis = None;
        Ortho::from_parts(dims, payload, up_axis)
    }

    pub fn id(&self) -> OrthoId {
        self.id
    }

    pub fn heap_bytes_estimate(&self) -> usize {
        // All fields are inline; no heap allocation
        std::mem::size_of::<Ortho>()
    }

    pub fn archived_id(archived: &rkyv::Archived<Ortho>) -> OrthoId {
        archived.id
    }
    pub fn get_current_position(&self) -> usize {
        self.next_empty as usize
    }

    /// If `add(value)` would trigger the `expand_up` path, returns `Some(insert_axis)`.
    /// Returns `None` for normal in-fill and expand-over completions.
    pub fn expanding_insert_axis(&self, value: PayloadVal) -> Option<usize> {
        let total_empty = self.payload_cap as usize - self.fill_count as usize;
        if total_empty == 1 && spatial::is_base(self.dims()) {
            Some(self.get_insert_position(value))
        } else {
            None
        }
    }
    pub fn add(&self, value: PayloadVal) -> Vec<Self> {
        let mut out = Vec::new();
        self.add_into(value, &mut out);
        out
    }

    pub fn add_into(&self, value: PayloadVal, out: &mut Vec<Self>) {
        out.reserve(1);
        let insertion_index = self.get_current_position();
        let total_empty = self.payload_cap as usize - self.fill_count as usize;
        if total_empty == 1 {
            if spatial::is_base(self.dims()) {
                let insert_axis = self.get_insert_position(value);
                Self::expand_up_into(self, value, insertion_index, insert_axis, out);
                return;
            } else {
                Self::expand_over_into(self, value, insertion_index, out);
                return;
            }
        }
        if insertion_index == 2 && self.dims() == &[2u8, 2u8] {
            let mut new_payload = self.payload;
            new_payload[insertion_index] = value;
            if new_payload[1] != EMPTY_CELL
                && new_payload[2] != EMPTY_CELL
                && new_payload[1] > new_payload[2]
            {
                new_payload.swap(1, 2);
            }
            let id = Self::compute_id(
                self.dims(),
                &new_payload[..self.payload_cap as usize],
                self.up_axis,
            );
            out.push(self.in_fill_child_from_payload_raw(new_payload, id));
            return;
        }
        let mut new_payload = self.payload;
        if insertion_index < self.payload_cap as usize {
            new_payload[insertion_index] = value;
        }
        let id = if insertion_index < self.payload_cap as usize {
            (self.id ^ Self::payload_cell_id(insertion_index, value)) & 0x7FFF_FFFF_FFFF_FFFF
        } else {
            Self::compute_id(
                self.dims(),
                &new_payload[..self.payload_cap as usize],
                self.up_axis,
            )
        };
        out.push(self.in_fill_child_from_payload_raw(new_payload, id));
    }

    fn expand_over_into(
        ortho: &Ortho,
        value: PayloadVal,
        insertion_index: usize,
        out: &mut Vec<Ortho>,
    ) {
        spatial::for_each_expand_over(ortho.dims(), |new_dims, new_capacity, reorg| {
            debug_assert!(new_capacity <= MAX_PAYLOAD);
            let mut new_payload = [EMPTY_CELL; MAX_PAYLOAD];
            for (i, &pos) in reorg.iter().enumerate() {
                new_payload[pos] = if i == insertion_index {
                    value
                } else if i < ortho.payload_cap as usize {
                    ortho.payload[i]
                } else {
                    EMPTY_CELL
                };
            }
            out.push(Ortho::from_parts_raw(
                new_dims,
                new_payload,
                new_capacity as u8,
                None,
            ));
        });
    }

    fn expand_up_into(
        ortho: &Ortho,
        value: PayloadVal,
        insertion_index: usize,
        insert_axis: usize,
        out: &mut Vec<Ortho>,
    ) {
        spatial::for_each_expand_up(
            ortho.dims(),
            insert_axis,
            |new_dims, new_capacity, reorg| {
                debug_assert!(new_capacity <= MAX_PAYLOAD);
                let mut new_payload = [EMPTY_CELL; MAX_PAYLOAD];
                for (i, &pos) in reorg.iter().enumerate() {
                    new_payload[pos] = if i == insertion_index {
                        value
                    } else if i < ortho.payload_cap as usize {
                        ortho.payload[i]
                    } else {
                        EMPTY_CELL
                    };
                }
                let is_up_child = new_dims.len() > ortho.dims().len();
                let up_axis = if is_up_child {
                    Some(Dim::try_from(insert_axis).expect("insert axis overflowed u8"))
                } else {
                    None
                };
                out.push(Ortho::from_parts_raw(
                    new_dims,
                    new_payload,
                    new_capacity as u8,
                    up_axis,
                ));
            },
        );
    }

    fn get_insert_position(&self, to_add: PayloadVal) -> usize {
        let mut idx = 0;
        for pos in 1..=self.dims_len as usize {
            let v = self.payload[pos];
            if v != EMPTY_CELL {
                if to_add < v {
                    return idx;
                }
                idx += 1;
            }
        }
        idx
    }

    pub fn fill_requirements_usize(
        &self,
        forbidden_out: &mut Vec<usize>,
        required_out: &mut Vec<Vec<usize>>,
    ) {
        let pos = self.get_current_position();
        let payload = &self.payload[..self.payload_cap as usize];
        spatial::with_requirements(pos, self.dims(), self.up_axis, |prefixes, diagonals| {
            Self::fill_from_meta_data(payload, prefixes, diagonals, forbidden_out, required_out);
        });
    }

    pub(crate) fn fill_requirements_usize_with_meta(
        &self,
        meta: &std::rc::Rc<spatial::DimMeta>,
        forbidden_out: &mut Vec<usize>,
        required_out: &mut Vec<Vec<usize>>,
    ) {
        let pos = self.get_current_position();
        let payload = &self.payload[..self.payload_cap as usize];
        spatial::with_meta_requirements(meta, pos, |prefixes, diagonals| {
            Self::fill_from_meta_data(payload, prefixes, diagonals, forbidden_out, required_out);
        });
    }

    pub(crate) fn fill_requirements_flat_with_meta(
        &self,
        meta: &std::rc::Rc<spatial::DimMeta>,
        forbidden_out: &mut Vec<usize>,
        required_ranges_out: &mut Vec<(usize, usize)>,
        required_values_out: &mut Vec<usize>,
    ) {
        let pos = self.get_current_position();
        let payload = &self.payload[..self.payload_cap as usize];
        spatial::with_meta_requirements(meta, pos, |prefixes, diagonals| {
            Self::fill_flat_from_meta_data(
                payload,
                prefixes,
                diagonals,
                forbidden_out,
                required_ranges_out,
                required_values_out,
            );
        });
    }

    fn fill_from_meta_data(
        payload: &[PayloadVal],
        prefixes: &[Vec<usize>],
        diagonals: &[usize],
        forbidden_out: &mut Vec<usize>,
        required_out: &mut Vec<Vec<usize>>,
    ) {
        forbidden_out.clear();
        for &idx in diagonals {
            if idx < payload.len() {
                let v = payload[idx];
                if v != EMPTY_CELL {
                    forbidden_out.push(payload_to_usize(v));
                }
            }
        }

        let mut out_idx = 0usize;
        for prefix in prefixes {
            if prefix.is_empty() {
                continue;
            }
            if out_idx == required_out.len() {
                required_out.push(Vec::with_capacity(prefix.len()));
            }
            let out = &mut required_out[out_idx];
            out.clear();
            if out.capacity() < prefix.len() {
                out.reserve(prefix.len() - out.capacity());
            }
            for &idx in prefix {
                if idx < payload.len() {
                    let v = payload[idx];
                    if v != EMPTY_CELL {
                        out.push(payload_to_usize(v));
                    }
                }
            }
            out_idx += 1;
        }
        required_out.truncate(out_idx);
    }

    fn fill_flat_from_meta_data(
        payload: &[PayloadVal],
        prefixes: &[Vec<usize>],
        diagonals: &[usize],
        forbidden_out: &mut Vec<usize>,
        required_ranges_out: &mut Vec<(usize, usize)>,
        required_values_out: &mut Vec<usize>,
    ) {
        forbidden_out.clear();
        for &idx in diagonals {
            if idx < payload.len() {
                let v = payload[idx];
                if v != EMPTY_CELL {
                    forbidden_out.push(payload_to_usize(v));
                }
            }
        }

        required_ranges_out.clear();
        required_values_out.clear();
        for prefix in prefixes {
            if prefix.is_empty() {
                continue;
            }
            let start = required_values_out.len();
            for &idx in prefix {
                if idx < payload.len() {
                    let v = payload[idx];
                    if v != EMPTY_CELL {
                        required_values_out.push(payload_to_usize(v));
                    }
                }
            }
            required_ranges_out.push((start, required_values_out.len() - start));
        }
    }

    pub fn get_requirements(&self) -> (Vec<PayloadVal>, Vec<Vec<PayloadVal>>) {
        let pos = self.get_current_position();
        let (prefixes, diagonals) = spatial::get_requirements(pos, self.dims(), self.up_axis);
        let cap = self.payload_cap as usize;
        let forbidden: Vec<PayloadVal> = diagonals
            .into_iter()
            .filter_map(|i| {
                if i < cap {
                    let v = self.payload[i];
                    if v != EMPTY_CELL { Some(v) } else { None }
                } else {
                    None
                }
            })
            .collect();
        let required: Vec<Vec<PayloadVal>> = prefixes
            .into_iter()
            .filter(|prefix| !prefix.is_empty())
            .map(|prefix| {
                prefix
                    .iter()
                    .filter_map(|&i| {
                        if i < cap {
                            let v = self.payload[i];
                            if v != EMPTY_CELL { Some(v) } else { None }
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<PayloadVal>>()
            })
            .collect();
        (forbidden, required)
    }

    pub fn get_requirement_phrases(&self) -> Vec<Vec<PayloadVal>> {
        let (_forbidden, required) = self.get_requirements();
        required
    }

    /// Remap an ortho's payload to use new vocabulary indices
    pub fn remap(&self, vocab_map: &[usize]) -> Option<Self> {
        let cap = self.payload_cap as usize;
        let mut new_payload = [EMPTY_CELL; MAX_PAYLOAD];
        for i in 0..cap {
            let v = self.payload[i];
            if v != EMPTY_CELL {
                let idx = payload_to_usize(v);
                let mapped = vocab_map[idx];
                new_payload[i] =
                    PayloadVal::try_from(mapped).expect("vocab map value overflowed u32");
            }
        }
        Some(Ortho::from_parts_raw(
            self.dims(),
            new_payload,
            self.payload_cap,
            self.up_axis,
        ))
    }

    pub fn prefixes(&self) -> Vec<Vec<PayloadVal>> {
        let cap = self.payload_cap as usize;
        let mut result = Vec::new();
        for pos in 0..cap {
            let (prefixes, _diagonals) = spatial::get_requirements(pos, self.dims(), self.up_axis);
            for prefix in prefixes {
                if !prefix.is_empty() {
                    let values: Vec<PayloadVal> = prefix
                        .iter()
                        .filter_map(|&i| {
                            if i < cap {
                                let v = self.payload[i];
                                if v != EMPTY_CELL { Some(v) } else { None }
                            } else {
                                None
                            }
                        })
                        .collect();
                    if !values.is_empty() {
                        result.push(values);
                    }
                }
            }
        }
        result
    }
    pub fn prefixes_for_last_filled(&self) -> Vec<Vec<PayloadVal>> {
        if self.get_current_position() == 0 {
            return vec![];
        }
        let pos = self.get_current_position() - 1;
        let cap = self.payload_cap as usize;
        let (prefixes, _diagonals) = spatial::get_requirements(pos, self.dims(), self.up_axis);
        prefixes
            .into_iter()
            .filter(|prefix| !prefix.is_empty())
            .map(|prefix| {
                prefix
                    .iter()
                    .filter_map(|&i| {
                        if i < cap {
                            let v = self.payload[i];
                            if v != EMPTY_CELL { Some(v) } else { None }
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<PayloadVal>>()
            })
            .filter(|v| !v.is_empty())
            .collect()
    }
    pub fn dims(&self) -> &[Dim] {
        &self.dims[..self.dims_len as usize]
    }
    pub fn payload_raw(&self) -> &[PayloadVal] {
        &self.payload[..self.payload_cap as usize]
    }
    pub fn payload_at(&self, idx: usize) -> Option<PayloadVal> {
        let v = self.payload[idx];
        if v != EMPTY_CELL { Some(v) } else { None }
    }
    pub fn payload_len(&self) -> usize {
        self.payload_cap as usize
    }
    pub fn up_axis(&self) -> Option<Dim> {
        self.up_axis
    }
    pub fn score(&self) -> OrthoScore {
        self.score
    }
    pub fn volume(&self) -> usize {
        self.score.volume
    }
    pub fn fullness(&self) -> usize {
        self.fill_count as usize
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, FoldError> {
        rkyv::to_bytes::<_, 256>(self)
            .map(|buf| buf.to_vec())
            .map_err(|e| FoldError::Serialization(e.to_string()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FoldError> {
        rkyv::from_bytes::<Ortho>(bytes).map_err(|e| FoldError::Deserialization(e.to_string()))
    }

    fn get_index_at_coord(&self, coord: &[usize]) -> Option<usize> {
        spatial::get_location_to_index(self.dims())
            .get(coord)
            .copied()
    }
}

pub struct OrthoDisplay<'a> {
    ortho: &'a Ortho,
    interner: &'a crate::interner::Interner,
}

impl<'a> OrthoDisplay<'a> {
    pub fn new(ortho: &'a Ortho, interner: &'a crate::interner::Interner) -> Self {
        Self { ortho, interner }
    }
}

impl<'a> fmt::Display for OrthoDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dims = self.ortho.dims();
        let rows = dim_to_usize(dims[dims.len() - 2]);
        let cols = dim_to_usize(dims[dims.len() - 1]);
        let higher_dims = &dims[..dims.len() - 2];

        let max_width = self
            .ortho
            .payload_raw()
            .iter()
            .filter(|&&v| v != EMPTY_CELL)
            .map(|&token_id| {
                self.interner
                    .string_for_index(payload_to_usize(token_id))
                    .len()
            })
            .max()
            .unwrap_or(1)
            .max(4);

        let format_cell = |token_id: Option<PayloadVal>| -> String {
            token_id
                .map(|id| {
                    format!(
                        "{:>width$}",
                        self.interner.string_for_index(payload_to_usize(id)),
                        width = max_width
                    )
                })
                .unwrap_or_else(|| format!("{:>width$}", "·", width = max_width))
        };

        let format_2d_slice = |prefix: &[usize]| -> String {
            (0..rows)
                .map(|row| {
                    (0..cols)
                        .map(|col| {
                            let coords: Vec<usize> =
                                prefix.iter().copied().chain([row, col]).collect();
                            self.ortho
                                .get_index_at_coord(&coords)
                                .filter(|&idx| idx < self.ortho.payload_len())
                                .and_then(|idx| self.ortho.payload_at(idx))
                                .map(|token_id| format_cell(Some(token_id)))
                                .unwrap_or_else(|| format_cell(None))
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        if higher_dims.is_empty() {
            return write!(f, "{}", format_2d_slice(&[]));
        }

        let tile_coords = Ortho::generate_tile_coords(higher_dims);

        let output = tile_coords
            .iter()
            .enumerate()
            .map(|(tile_idx, coords)| {
                let separator = if tile_idx > 0 { "\n\n" } else { "" };
                let dims_str = coords
                    .iter()
                    .enumerate()
                    .map(|(i, &val)| format!("dim{}={}", i, val))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}[{}]\n{}", separator, dims_str, format_2d_slice(coords))
            })
            .collect::<Vec<_>>()
            .join("");

        write!(f, "{}", output)
    }
}

impl Ortho {
    pub fn display<'a>(&'a self, interner: &'a crate::interner::Interner) -> OrthoDisplay<'a> {
        OrthoDisplay::new(self, interner)
    }

    fn generate_tile_coords(dims: &[Dim]) -> Vec<Vec<usize>> {
        if dims.is_empty() {
            return vec![vec![]];
        }

        let dims_usize: Vec<usize> = dims.iter().map(|&d| dim_to_usize(d)).collect();
        let total: usize = dims_usize.iter().product();
        (0..total)
            .map(|mut idx| {
                let mut coord = Vec::with_capacity(dims_usize.len());
                for &dim_size in dims_usize.iter().rev() {
                    coord.push(idx % dim_size);
                    idx /= dim_size;
                }
                coord.reverse();
                coord
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_ortho(dims: Vec<usize>, payload: Vec<Option<usize>>, up_axis: Option<usize>) -> Ortho {
        let dims_u8: Vec<Dim> = dims
            .into_iter()
            .map(|d| Dim::try_from(d).expect("dim overflowed u8"))
            .collect();
        let payload_u32: Vec<Option<PayloadVal>> = payload
            .into_iter()
            .map(|v| v.map(|x| PayloadVal::try_from(x).expect("payload value overflowed u32")))
            .collect();
        Ortho::from_parts(
            &dims_u8,
            &payload_u32,
            up_axis.map(|a| Dim::try_from(a).expect("up_axis overflowed u8")),
        )
    }

    #[test]
    fn test_new() {
        let ortho = Ortho::new();
        assert_eq!(ortho.dims(), &[2u8, 2u8]);
        assert_eq!(ortho.payload_len(), 4);
        assert!(ortho.payload_raw().iter().all(|&v| v == EMPTY_CELL));
        assert_eq!(ortho.volume(), 1, "volume should be (2-1)*(2-1) = 1");
        assert_eq!(
            ortho.fullness(),
            0,
            "fullness should be 0 (no filled slots)"
        );
        assert_eq!(
            ortho.score(),
            OrthoScore {
                volume: 1,
                variance_num: 0,
                variance_den: 4,
                fullness: 0,
            }
        );
    }

    #[test]
    fn test_get_current() {
        let ortho = Ortho::new();
        assert_eq!(ortho.get_current_position(), 0);

        assert_eq!(
            mk_ortho(vec![2, 2], vec![None, None, None, None], None).get_current_position(),
            0
        );

        assert_eq!(
            mk_ortho(vec![2, 2], vec![Some(1), None, None, None], None).get_current_position(),
            1
        );

        assert_eq!(
            mk_ortho(vec![2, 2], vec![Some(1), Some(2), None, None], None).get_current_position(),
            2
        );

        assert_eq!(
            mk_ortho(vec![2, 2], vec![Some(1), Some(2), Some(3), None], None)
                .get_current_position(),
            3
        );
    }

    #[test]
    fn test_get_insert_position() {
        let ortho = Ortho::new();
        assert_eq!(ortho.get_insert_position(5), 0);

        assert_eq!(
            mk_ortho(vec![2, 2], vec![Some(0), Some(15), None, None], None).get_insert_position(14),
            0
        );

        assert_eq!(
            mk_ortho(vec![2, 2], vec![Some(0), Some(15), None, None], None).get_insert_position(20),
            1
        );

        assert_eq!(
            mk_ortho(vec![2, 2], vec![Some(0), Some(10), Some(20), None], None)
                .get_insert_position(5),
            0
        );

        assert_eq!(
            mk_ortho(vec![2, 2], vec![Some(0), Some(10), Some(20), None], None)
                .get_insert_position(15),
            1
        );

        assert_eq!(
            mk_ortho(vec![2, 2], vec![Some(0), Some(10), Some(20), None], None)
                .get_insert_position(1000),
            2
        );
    }

    #[test]
    fn test_add_simple() {
        let ortho = Ortho::new();
        let orthos = ortho.add(10);
        assert_eq!(
            orthos,
            vec![mk_ortho(vec![2, 2], vec![Some(10), None, None, None], None)]
        );
    }

    #[test]
    fn test_add_multiple() {
        let ortho = Ortho::new();
        let orthos1 = ortho.add(1);
        let ortho = &orthos1[0];
        let orthos2 = ortho.add(2);
        assert_eq!(
            orthos2,
            vec![mk_ortho(
                vec![2, 2],
                vec![Some(1), Some(2), None, None],
                None
            )]
        );
    }

    #[test]
    fn test_add_order_independent_ids() {
        let ortho1 = Ortho::new();
        let ortho2 = Ortho::new();
        let ortho1 = &ortho1.add(1)[0];
        let ortho2 = &ortho2.add(1)[0];
        assert_eq!(ortho1.id(), ortho2.id());
        let ortho1 = &ortho1.add(2)[0];
        let ortho2 = &ortho2.add(3)[0];
        assert_ne!(ortho1.id(), ortho2.id());
        let ortho1 = &ortho1.add(3)[0];
        let ortho2 = &ortho2.add(2)[0];
        assert_eq!(ortho1.id(), ortho2.id());
        let ortho1 = &ortho1.add(4)[0];
        let ortho2 = &ortho2.add(4)[0];
        assert_eq!(ortho1.id(), ortho2.id());
    }

    #[test]
    fn test_add_shape_expansion() {
        let ortho = Ortho::new();
        let orthos = ortho.add(1);
        let ortho = &orthos[0];
        let orthos2 = ortho.add(2);
        let ortho = &orthos2[0];
        assert_eq!(ortho.dims(), &[2u8, 2u8]);
        assert_eq!(ortho.payload_at(0), Some(1));
        assert_eq!(ortho.payload_at(1), Some(2));
        assert_eq!(ortho.payload_at(2), None);
        assert_eq!(ortho.payload_at(3), None);
        let orthos3 = ortho.add(3);
        let ortho = &orthos3[0];
        assert_eq!(ortho.dims(), &[2u8, 2u8]);
        assert_eq!(ortho.payload_at(0), Some(1));
        assert_eq!(ortho.payload_at(1), Some(2));
        assert_eq!(ortho.payload_at(2), Some(3));
        assert_eq!(ortho.payload_at(3), None);
    }

    #[test]
    fn test_up_and_over_expansions_full_coverage() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(1)[0];
        let ortho = &ortho.add(2)[0];
        let ortho = &ortho.add(3)[0];

        let expansions = ortho.add(4);
        assert_eq!(
            expansions,
            vec![
                mk_ortho(
                    vec![2, 3],
                    vec![Some(1), Some(2), Some(3), None, Some(4), None],
                    None
                ), // Over expansion
                mk_ortho(
                    vec![2, 2, 2],
                    vec![Some(1), Some(2), Some(3), None, Some(4), None, None, None],
                    Some(2)
                ) // Up expansion at axis 2
            ]
        );
    }

    #[test]
    fn test_insert_position_middle() {
        let ortho = mk_ortho(vec![2, 2], vec![Some(10), Some(20), None, None], None);
        let orthos = ortho.add(15);
        assert_eq!(
            orthos,
            vec![mk_ortho(
                vec![2, 2],
                vec![Some(10), Some(15), Some(20), None],
                None
            )]
        );
    }

    #[test]
    fn test_insert_position_middle_and_reorg() {
        let ortho = mk_ortho(vec![2, 2], vec![Some(10), None, Some(20), Some(30)], None);

        let mut orthos = ortho.add(15);
        orthos.sort_by(|a, b| a.dims().cmp(b.dims()));
        assert_eq!(
            orthos,
            vec![
                mk_ortho(
                    vec![2, 2, 2],
                    vec![
                        Some(10),
                        None,
                        Some(15),
                        Some(20),
                        None,
                        None,
                        Some(30),
                        None
                    ],
                    Some(0)
                ), // Up expansion at axis 0
                mk_ortho(
                    vec![2, 3],
                    vec![Some(10), Some(15), Some(20), None, Some(30), None],
                    None
                ), // Over expansion
            ]
        );
    }

    #[test]
    fn test_get_requirements_empty() {
        let ortho = Ortho::new();
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<PayloadVal>::new());
        assert_eq!(required, Vec::<Vec<PayloadVal>>::new());
    }

    #[test]
    fn test_get_requirements_simple() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(10)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<PayloadVal>::new());
        assert_eq!(required, vec![vec![10]]);
    }

    #[test]
    fn test_get_requirements_multiple() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, vec![20]);
        assert_eq!(required, vec![vec![10]]);
    }

    #[test]
    fn test_get_requirements_full() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];
        let ortho = &ortho.add(30)[0];
        let ortho = &ortho.add(40)[0];

        // With sorted dims [2,3] instead of old [3,2]:
        // payload = [Some(10), Some(20), Some(30), None, Some(40), None]
        // current_position = 3 (first None)
        // At position 3 (index [0,2], distance 2):
        // Position 4 (index [1,1], also distance 2) is in the same shell
        // Position 4 has content (40) from the reorg, so 40 is forbidden
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, vec![40]);
        assert_eq!(required, vec![vec![10, 20]]);
    }

    #[test]
    fn test_get_requirements_expansion() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(1)[0];
        let ortho = &ortho.add(2)[0];
        let ortho = &ortho.add(3)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<PayloadVal>::new());
        assert_eq!(required, vec![vec![2], vec![3]]);
    }

    #[test]
    fn test_get_requirements_order_independent() {
        let ortho1 = Ortho::new();
        let ortho2 = Ortho::new();
        let ortho1 = &ortho1.add(1)[0];
        let ortho2 = &ortho2.add(1)[0];
        let ortho1 = &ortho1.add(2)[0];
        let ortho2 = &ortho2.add(3)[0];
        let ortho1 = &ortho1.add(3)[0];
        let ortho2 = &ortho2.add(2)[0];
        let (forbidden1, required1) = ortho1.get_requirements();
        let (forbidden2, required2) = ortho2.get_requirements();
        assert_eq!(forbidden1, forbidden2);
        assert_eq!(required1, vec![vec![2], vec![3]]);
        assert_eq!(required2, vec![vec![2], vec![3]]);
    }

    #[test]
    fn test_id_version_behavior() {
        // Test that orthos with same contents have same IDs
        let ortho_with_content_1 = mk_ortho(vec![2, 2], vec![Some(10), None, None, None], None);
        let ortho_with_content_2 = mk_ortho(vec![2, 2], vec![Some(10), None, None, None], None);
        assert_eq!(ortho_with_content_1.id(), ortho_with_content_2.id());

        // Test that orthos with different contents have different IDs
        let ortho_content_a = mk_ortho(vec![2, 2], vec![Some(10), None, None, None], None);
        let ortho_content_b = mk_ortho(vec![2, 2], vec![Some(20), None, None, None], None);
        assert_ne!(ortho_content_a.id(), ortho_content_b.id());
    }

    #[test]
    fn test_id_collision_for_different_payloads() {
        // These are the payloads seen in the logs
        let ortho0 = mk_ortho(vec![2, 2], vec![Some(0), None, None, None], None);
        let ortho1 = mk_ortho(vec![2, 2], vec![Some(1), None, None, None], None);
        let ortho2 = mk_ortho(vec![2, 2], vec![Some(2), None, None, None], None);
        let ortho3 = mk_ortho(vec![2, 2], vec![Some(3), None, None, None], None);
        let ortho4 = mk_ortho(vec![2, 2], vec![Some(4), None, None, None], None);
        let ortho5 = mk_ortho(vec![2, 2], vec![Some(5), None, None, None], None);
        let ids = vec![
            ortho0.id(),
            ortho1.id(),
            ortho2.id(),
            ortho3.id(),
            ortho4.id(),
            ortho5.id(),
        ];
        // If there are collisions, there will be fewer unique IDs than payloads
        let unique_ids: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(
            unique_ids.len(),
            ids.len(),
            "Ortho::id() should be unique for different payloads, but got collisions: {:?}",
            ids
        );
    }

    #[test]
    fn test_canonicalization_invariant_axis_permutation() {
        // This test is intended to expose the canonicalization issue: inserting the two axis tokens
        // in different orders should yield (after inserting the 4th token that triggers expansion)
        // an equivalent canonical set of children. Currently (with the swap removed) they differ.
        // Axis tokens are the 2nd and 3rd overall inserts into base dims [2,2].
        // Path 1: a < b < c
        let mut o1 = Ortho::new();
        o1 = o1.add(10).pop().unwrap(); // a
        o1 = o1.add(20).pop().unwrap(); // b
        o1 = o1.add(30).pop().unwrap(); // c
        // Path 2: a < c but b < c (second and third swapped relative to path 1)
        let mut o2 = Ortho::new();
        o2 = o2.add(10).pop().unwrap(); // a
        o2 = o2.add(30).pop().unwrap(); // c
        o2 = o2.add(20).pop().unwrap(); // b (unsorted axis order)
        // Insert 4th token to force expansion candidates
        let children1 = o1.add(40);
        let children2 = o2.add(40);
        // Normalize each child to (dims, filled_values_in_order)
        fn norm(o: &Ortho) -> (Vec<Dim>, Vec<PayloadVal>) {
            (
                o.dims().to_vec(),
                o.payload_raw()
                    .iter()
                    .filter(|&&v| v != EMPTY_CELL)
                    .copied()
                    .collect(),
            )
        }
        let mut norms1: Vec<_> = children1.iter().map(norm).collect();
        let mut norms2: Vec<_> = children2.iter().map(norm).collect();
        norms1.sort();
        norms2.sort();
        assert_eq!(
            norms1, norms2,
            "Canonicalization mismatch between axis insertion orders. norms1={:?} norms2={:?}",
            norms1, norms2
        );
    }

    #[test]
    fn test_display_2d_simple() {
        use crate::interner::Interner;
        let interner = Interner::from_text("a b c d");
        let ortho = mk_ortho(vec![2, 2], vec![Some(0), Some(1), Some(2), Some(3)], None);
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "   a    b\n   c    d");
    }

    #[test]
    fn test_display_2d_with_nones() {
        use crate::interner::Interner;
        let interner = Interner::from_text("hello world");
        let ortho = mk_ortho(vec![2, 2], vec![Some(0), Some(1), None, None], None);
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "hello world\n    ·     ·");
    }

    #[test]
    fn test_display_3x2() {
        use crate::interner::Interner;
        let interner = Interner::from_text("a b c d e");
        let ortho = mk_ortho(
            vec![3, 2],
            vec![Some(0), Some(1), Some(2), Some(3), Some(4), None],
            None,
        );
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "   a    b\n   c    d\n   e    ·");
    }

    #[test]
    fn test_display_3d_tiled() {
        use crate::interner::Interner;
        let interner = Interner::from_text("a b c d e f g");
        let ortho = mk_ortho(
            vec![2, 2, 2],
            vec![
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(6),
                None,
            ],
            None,
        );
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(
            display_str,
            "[dim0=0]\n   a    b\n   c    e\n\n[dim0=1]\n   d    f\n   g    ·"
        );
    }

    #[test]
    fn test_get_requirement_phrases() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];

        let phrases = ortho.get_requirement_phrases();
        assert_eq!(phrases, vec![vec![10]]);

        let ortho = &ortho.add(30)[0];
        let ortho = &ortho.add(40)[0];
        // With sorted dims [2,3], the required phrases at position 3 are [[10, 20]]
        let phrases = ortho.get_requirement_phrases();
        assert_eq!(phrases, vec![vec![10, 20]]);
    }

    #[test]
    fn test_get_requirement_phrases_expansion() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(1)[0];
        let ortho = &ortho.add(2)[0];
        let ortho = &ortho.add(3)[0];

        let phrases = ortho.get_requirement_phrases();
        assert_eq!(phrases, vec![vec![2], vec![3]]);
    }

    #[test]
    fn test_cached_score_matches_computed() {
        fn compute_score_directly(ortho: &Ortho) -> OrthoScore {
            let volume = ortho
                .dims()
                .iter()
                .map(|x| usize::from(*x).saturating_sub(1))
                .product::<usize>();
            let dim_count = ortho.dims().len() as u128;
            let dim_sum = ortho.dims().iter().map(|&d| u128::from(d)).sum::<u128>();
            let dim_sum_sq = ortho
                .dims()
                .iter()
                .map(|&d| {
                    let value = u128::from(d);
                    value * value
                })
                .sum::<u128>();
            let fullness = ortho.fullness();
            OrthoScore {
                volume,
                variance_num: dim_count * dim_sum_sq - dim_sum * dim_sum,
                variance_den: dim_count * dim_count,
                fullness,
            }
        }

        // Test new ortho
        let ortho = Ortho::new();
        assert_eq!(
            ortho.score(),
            compute_score_directly(&ortho),
            "New ortho score mismatch"
        );

        // Test after adding values
        let ortho = ortho.add(1).pop().unwrap();
        assert_eq!(
            ortho.score(),
            compute_score_directly(&ortho),
            "After add(1) score mismatch"
        );

        let ortho = ortho.add(2).pop().unwrap();
        assert_eq!(
            ortho.score(),
            compute_score_directly(&ortho),
            "After add(2) score mismatch"
        );

        let ortho = ortho.add(3).pop().unwrap();
        assert_eq!(
            ortho.score(),
            compute_score_directly(&ortho),
            "After add(3) score mismatch"
        );

        // Test expansion - this returns multiple orthos
        let expansions = ortho.add(4);
        for (i, expanded_ortho) in expansions.iter().enumerate() {
            assert_eq!(
                expanded_ortho.score(),
                compute_score_directly(expanded_ortho),
                "Expansion {} score mismatch",
                i
            );
        }

        // Test remap
        let ortho_to_remap = Ortho::new().add(5).pop().unwrap();
        let vocab_map = vec![0, 1, 2, 3, 4, 5];
        if let Some(remapped) = ortho_to_remap.remap(&vocab_map) {
            assert_eq!(
                remapped.score(),
                compute_score_directly(&remapped),
                "Remapped ortho score mismatch"
            );
        }
    }

    #[test]
    fn lower_variance_beats_higher_fullness_on_equal_volume() {
        let squareish = mk_ortho(
            vec![3, 4],
            {
                let mut payload = vec![Some(1); 11];
                payload.push(None);
                payload
            },
            None,
        );
        let skinny = mk_ortho(
            vec![2, 7],
            {
                let mut payload = vec![Some(1); 13];
                payload.push(None);
                payload
            },
            None,
        );

        assert_eq!(
            squareish.volume(),
            skinny.volume(),
            "setup expects equal volume"
        );
        assert!(
            squareish.score() > skinny.score(),
            "lower variance should beat higher fullness at equal volume"
        );
    }

    #[test]
    fn fullness_breaks_ties_when_variance_matches() {
        let less_full = mk_ortho(
            vec![3, 4, 5],
            {
                let mut payload = vec![Some(1); 10];
                payload.extend(vec![None; 50]);
                payload
            },
            None,
        );
        let more_full = mk_ortho(
            vec![5, 4, 3],
            {
                let mut payload = vec![Some(1); 11];
                payload.extend(vec![None; 49]);
                payload
            },
            None,
        );

        assert_eq!(
            less_full.volume(),
            more_full.volume(),
            "setup expects equal volume"
        );
        assert_eq!(
            less_full.score().variance_cmp(&more_full.score()),
            Ordering::Equal,
            "setup expects equal variance"
        );
        assert!(
            more_full.score() > less_full.score(),
            "fullness should break ties when volume and variance match"
        );
    }

    #[test]
    fn serialization_preserves_cached_hot_fields() {
        let ortho = Ortho::new()
            .add(1)
            .pop()
            .unwrap()
            .add(2)
            .pop()
            .unwrap()
            .add(3)
            .pop()
            .unwrap();
        let bytes = ortho.to_bytes().unwrap();
        let decoded = Ortho::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.get_current_position(), ortho.get_current_position());
        assert_eq!(decoded.fullness(), ortho.fullness());
        assert_eq!(decoded.score(), ortho.score());
    }
}
