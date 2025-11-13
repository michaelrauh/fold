use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};

// We need to copy the implementation inline since we can't have multiple versions of the same module
mod ortho_current {
    use rustc_hash::FxHasher;
    use std::hash::{Hash, Hasher};

    #[derive(PartialEq, Debug, Clone)]
    pub struct Ortho {
        id: usize,
        dims: Vec<usize>,
        payload: Vec<Option<usize>>,
    }

    impl Ortho {
        fn compute_id(dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
            let mut hasher = FxHasher::default();
            dims.hash(&mut hasher);
            payload.hash(&mut hasher);
            (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
        }
        
        pub fn new() -> Self {
            let dims = vec![2,2];
            let payload = vec![None; 4];
            let id = Self::compute_id(&dims, &payload);
            Ortho { id, dims, payload }
        }
        
        pub fn id(&self) -> usize { self.id }
        
        pub fn get_current_position(&self) -> usize { 
            self.payload.iter().position(|x| x.is_none()).unwrap_or(self.payload.len()) 
        }
        
        pub fn add(&self, value: usize) -> Self {
            let insertion_index = self.get_current_position();
            let len = self.payload.len();
            let mut new_payload: Vec<Option<usize>> = Vec::with_capacity(len);
            unsafe { 
                new_payload.set_len(len); 
                std::ptr::copy_nonoverlapping(self.payload.as_ptr(), new_payload.as_mut_ptr(), len); 
            }
            if insertion_index < new_payload.len() { 
                new_payload[insertion_index] = Some(value); 
            }
            let new_id = Self::compute_id(&self.dims, &new_payload);
            Ortho { id: new_id, dims: self.dims.clone(), payload: new_payload }
        }
    }
}

mod ortho_no_id {
    use rustc_hash::FxHasher;
    use std::hash::{Hash, Hasher};

    #[derive(PartialEq, Debug, Clone)]
    pub struct Ortho {
        dims: Vec<usize>,
        payload: Vec<Option<usize>>,
    }

    impl Ortho {
        fn compute_id(dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
            let mut hasher = FxHasher::default();
            dims.hash(&mut hasher);
            payload.hash(&mut hasher);
            (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
        }
        
        pub fn new() -> Self {
            let dims = vec![2,2];
            let payload = vec![None; 4];
            Ortho { dims, payload }
        }
        
        pub fn id(&self) -> usize { 
            Self::compute_id(&self.dims, &self.payload)
        }
        
        pub fn get_current_position(&self) -> usize { 
            self.payload.iter().position(|x| x.is_none()).unwrap_or(self.payload.len()) 
        }
        
        pub fn add(&self, value: usize) -> Self {
            let insertion_index = self.get_current_position();
            let len = self.payload.len();
            let mut new_payload: Vec<Option<usize>> = Vec::with_capacity(len);
            unsafe { 
                new_payload.set_len(len); 
                std::ptr::copy_nonoverlapping(self.payload.as_ptr(), new_payload.as_mut_ptr(), len); 
            }
            if insertion_index < new_payload.len() { 
                new_payload[insertion_index] = Some(value); 
            }
            Ortho { dims: self.dims.clone(), payload: new_payload }
        }
    }
}

mod ortho_hybrid {
    use rustc_hash::FxHasher;
    use std::hash::{Hash, Hasher};

    #[derive(PartialEq, Debug, Clone)]
    pub struct Ortho {
        id: usize,
        dims: Vec<usize>,
        payload: Vec<Option<usize>>,
    }

    impl Ortho {
        fn compute_id_full(dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
            let mut hasher = FxHasher::default();
            dims.hash(&mut hasher);
            payload.hash(&mut hasher);
            (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
        }
        
        fn compute_id_incremental(parent_id: usize, value: usize) -> usize {
            let mut hasher = FxHasher::default();
            parent_id.hash(&mut hasher);
            value.hash(&mut hasher);
            (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
        }
        
        pub fn new() -> Self {
            let dims = vec![2,2];
            let payload = vec![None; 4];
            let id = Self::compute_id_full(&dims, &payload);
            Ortho { id, dims, payload }
        }
        
        pub fn id(&self) -> usize { self.id }
        
        pub fn get_current_position(&self) -> usize { 
            self.payload.iter().position(|x| x.is_none()).unwrap_or(self.payload.len()) 
        }
        
        pub fn add(&self, value: usize) -> Self {
            let insertion_index = self.get_current_position();
            let len = self.payload.len();
            let mut new_payload: Vec<Option<usize>> = Vec::with_capacity(len);
            unsafe { 
                new_payload.set_len(len); 
                std::ptr::copy_nonoverlapping(self.payload.as_ptr(), new_payload.as_mut_ptr(), len); 
            }
            if insertion_index < new_payload.len() { 
                new_payload[insertion_index] = Some(value); 
            }
            // Use incremental ID (no reordering in simple add)
            let new_id = Self::compute_id_incremental(self.id, value);
            Ortho { id: new_id, dims: self.dims.clone(), payload: new_payload }
        }
    }
}

fn bench_ortho_id(c: &mut Criterion) {
    let mut group = c.benchmark_group("ortho_id_comparison");
    group.sample_size(20);
    
    // Current approach (stored ID, computed on add)
    group.bench_function("current_stored_id", |b| {
        let ortho = ortho_current::Ortho::new();
        let ortho = ortho.add(10);
        let ortho = ortho.add(20);
        b.iter(|| black_box(ortho.id()));
    });
    
    // No ID field (compute on demand)
    group.bench_function("no_id_field", |b| {
        let ortho = ortho_no_id::Ortho::new();
        let ortho = ortho.add(10);
        let ortho = ortho.add(20);
        b.iter(|| black_box(ortho.id()));
    });
    
    // Hybrid (incremental ID)
    group.bench_function("hybrid_incremental", |b| {
        let ortho = ortho_hybrid::Ortho::new();
        let ortho = ortho.add(10);
        let ortho = ortho.add(20);
        b.iter(|| black_box(ortho.id()));
    });
    
    group.finish();
}

fn bench_ortho_add(c: &mut Criterion) {
    let mut group = c.benchmark_group("ortho_add_comparison");
    group.sample_size(20);
    
    // Current approach
    group.bench_function("current_add", |b| {
        let ortho = ortho_current::Ortho::new();
        b.iter(|| {
            let o = black_box(&ortho);
            black_box(o.add(10))
        });
    });
    
    // No ID field
    group.bench_function("no_id_add", |b| {
        let ortho = ortho_no_id::Ortho::new();
        b.iter(|| {
            let o = black_box(&ortho);
            black_box(o.add(10))
        });
    });
    
    // Hybrid
    group.bench_function("hybrid_add", |b| {
        let ortho = ortho_hybrid::Ortho::new();
        b.iter(|| {
            let o = black_box(&ortho);
            black_box(o.add(10))
        });
    });
    
    group.finish();
}

fn bench_worker_loop_simulation(c: &mut Criterion) {
    let mut group = c.benchmark_group("worker_loop_simulation");
    group.sample_size(20);
    
    // Current approach
    group.bench_function("current", |b| {
        b.iter(|| {
            let ortho = ortho_current::Ortho::new();
            let child = ortho.add(10);
            let child_id = black_box(child.id());
            black_box(child_id)
        });
    });
    
    // No ID field
    group.bench_function("no_id", |b| {
        b.iter(|| {
            let ortho = ortho_no_id::Ortho::new();
            let child = ortho.add(10);
            let child_id = black_box(child.id());
            black_box(child_id)
        });
    });
    
    // Hybrid
    group.bench_function("hybrid", |b| {
        b.iter(|| {
            let ortho = ortho_hybrid::Ortho::new();
            let child = ortho.add(10);
            let child_id = black_box(child.id());
            black_box(child_id)
        });
    });
    
    group.finish();
}

fn bench_memory_footprint(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_footprint");
    group.sample_size(10);
    
    // Current approach
    group.bench_function("current_size", |b| {
        b.iter(|| {
            let ortho = ortho_current::Ortho::new();
            black_box(std::mem::size_of_val(&ortho))
        });
    });
    
    // No ID field
    group.bench_function("no_id_size", |b| {
        b.iter(|| {
            let ortho = ortho_no_id::Ortho::new();
            black_box(std::mem::size_of_val(&ortho))
        });
    });
    
    // Hybrid
    group.bench_function("hybrid_size", |b| {
        b.iter(|| {
            let ortho = ortho_hybrid::Ortho::new();
            black_box(std::mem::size_of_val(&ortho))
        });
    });
    
    group.finish();
}

criterion_group!(benches, bench_ortho_id, bench_ortho_add, bench_worker_loop_simulation, bench_memory_footprint);
criterion_main!(benches);
