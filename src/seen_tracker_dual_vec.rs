use crate::FoldError;
use bloomfilter::Bloom;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;

/// Two-tier tracker: one large in-memory sorted Vec plus a single persisted run on disk.
/// Flush merges the in-memory Vec into the on-disk run in one pass.
pub struct DualVecSeenTracker {
    bloom: Bloom<usize>,
    active: Vec<usize>,
    active_sorted: bool,
    disk: Vec<usize>,
    flush_limit: usize,
    run_path: PathBuf,
    total_seen_count: usize,
}

impl DualVecSeenTracker {
    pub fn new(expected_items: usize, flush_limit: usize) -> Self {
        Self::with_path("./fold_state/seen_dual_vec.bin", expected_items, flush_limit)
    }

    pub fn with_path(path: &str, bloom_capacity: usize, flush_limit: usize) -> Self {
        let bloom_capacity = bloom_capacity.max(1_000_000);
        let bloom = Bloom::new_for_fp_rate(bloom_capacity, 0.01);
        Self {
            bloom,
            active: Vec::new(),
            active_sorted: false,
            disk: Vec::new(),
            flush_limit,
            run_path: PathBuf::from(path),
            total_seen_count: 0,
        }
    }

    fn ensure_active_sorted(&mut self) {
        if !self.active_sorted {
            self.active.sort_unstable();
            self.active.dedup();
            self.active_sorted = true;
        }
    }

    fn merge_to_disk(&mut self) -> Result<(), FoldError> {
        self.ensure_active_sorted();
        if self.active.is_empty() && self.disk.is_empty() {
            return Ok(());
        }

        let mut merged = Vec::with_capacity(self.active.len() + self.disk.len());
        let mut i = 0;
        let mut j = 0;
        while i < self.active.len() && j < self.disk.len() {
            let a = self.active[i];
            let b = self.disk[j];
            if a < b {
                merged.push(a);
                i += 1;
            } else if b < a {
                merged.push(b);
                j += 1;
            } else {
                merged.push(a);
                i += 1;
                j += 1;
            }
        }
        if i < self.active.len() {
            merged.extend_from_slice(&self.active[i..]);
        }
        if j < self.disk.len() {
            merged.extend_from_slice(&self.disk[j..]);
        }

        self.write_run(&merged)?;
        self.disk = merged;
        self.active.clear();
        self.active_sorted = false;
        self.total_seen_count = self.disk.len();
        Ok(())
    }

    fn write_run(&self, ids: &[usize]) -> Result<(), FoldError> {
        if let Some(dir) = self.run_path.parent() {
            fs::create_dir_all(dir).map_err(FoldError::Io)?;
        }
        let mut file = File::create(&self.run_path).map_err(FoldError::Io)?;
        let count = ids.len() as u64;
        file.write_all(&count.to_le_bytes())
            .map_err(FoldError::Io)?;
        for id in ids {
            file.write_all(&(*id as u64).to_le_bytes())
                .map_err(FoldError::Io)?;
        }
        file.flush().map_err(FoldError::Io)?;
        Ok(())
    }

    #[allow(dead_code)]
    fn load_run(&mut self) -> Result<(), FoldError> {
        if !self.run_path.exists() {
            return Ok(());
        }
        let mut file = File::open(&self.run_path).map_err(FoldError::Io)?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header).map_err(FoldError::Io)?;
        let count = u64::from_le_bytes(header) as usize;
        let mut buf = vec![0u8; count * 8];
        file.read_exact(&mut buf).map_err(FoldError::Io)?;
        self.disk.clear();
        self.disk.reserve(count);
        for chunk in buf.chunks_exact(8) {
            self.disk
                .push(u64::from_le_bytes(chunk.try_into().unwrap()) as usize);
        }
        self.total_seen_count = self.disk.len();
        Ok(())
    }

    pub fn contains(&mut self, id: &usize) -> bool {
        if !self.bloom.check(id) {
            return false;
        }
        if !self.active_sorted {
            self.ensure_active_sorted();
        }
        if self.active.binary_search(id).is_ok() {
            return true;
        }
        self.disk.binary_search(id).is_ok()
    }

    pub fn insert(&mut self, id: usize) {
        if self.contains(&id) {
            return;
        }
        self.bloom.set(&id);
        self.active.push(id);
        self.active_sorted = false;
        if self.active.len() >= self.flush_limit {
            let _ = self.merge_to_disk();
        }
        self.total_seen_count = self.total_seen_count.saturating_add(1);
    }

    pub fn flush(&mut self) -> Result<(), FoldError> {
        self.merge_to_disk()
    }

    pub fn len(&self) -> usize {
        self.total_seen_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn dual_vec_basic() {
        let mut tracker = DualVecSeenTracker::with_path("./fold_state/dual_vec_test.bin", 1000, 10);
        tracker.insert(1);
        tracker.insert(2);
        assert!(tracker.contains(&1));
        assert!(tracker.contains(&2));
        tracker.flush().unwrap();
        assert!(tracker.contains(&1));
        assert!(tracker.contains(&2));
    }

    #[test]
    fn dual_vec_flush_merges() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("dual.bin");
        let mut tracker = DualVecSeenTracker::with_path(path.to_str().unwrap(), 1000, 4);
        for i in 0..8 {
            tracker.insert(i);
        }
        tracker.flush().unwrap();
        assert_eq!(tracker.len(), 8);
        for i in 0..8 {
            assert!(tracker.contains(&i));
        }
    }
}
