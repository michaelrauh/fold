use crate::FoldError;
use bloomfilter::Bloom;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

#[derive(Clone, Debug)]
struct RunIndexEntry {
    first_key: usize,
    last_key: usize,
    offset: u64,
    len: usize,
}

#[derive(Debug)]
struct RunFile {
    index: Vec<RunIndexEntry>,
    file: File,
    cache: RunBlockCache,
}

#[derive(Default, Debug)]
struct RunBlockCache {
    offset: u64,
    ids: Vec<usize>,
}

impl RunBlockCache {
    fn new() -> Self {
        Self {
            offset: u64::MAX,
            ids: Vec::new(),
        }
    }
}

impl RunFile {
    fn new(path: PathBuf, index: Vec<RunIndexEntry>) -> Result<Self, FoldError> {
        let file = File::open(&path).map_err(FoldError::Io)?;
        Ok(Self {
            index,
            file,
            cache: RunBlockCache::new(),
        })
    }

    fn load_block<'a>(
        &'a mut self,
        entry: &RunIndexEntry,
        scratch: &mut Vec<u8>,
    ) -> Result<&'a [usize], FoldError> {
        if self.cache.offset == entry.offset && !self.cache.ids.is_empty() {
            return Ok(&self.cache.ids);
        }

        let byte_len = entry.len.saturating_mul(8);
        if scratch.len() < byte_len {
            scratch.resize(byte_len, 0);
        }
        let buf = &mut scratch[..byte_len];

        self.file
            .seek(SeekFrom::Start(entry.offset))
            .map_err(FoldError::Io)?;
        self.file.read_exact(buf).map_err(FoldError::Io)?;

        self.cache.ids.clear();
        self.cache.ids.reserve(entry.len);
        for chunk in buf.chunks_exact(8) {
            self.cache
                .ids
                .push(u64::from_le_bytes(chunk.try_into().unwrap()) as usize);
        }
        self.cache.offset = entry.offset;
        Ok(&self.cache.ids)
    }
}

/// Run-based tracker that keeps a configurable number of recent runs fully in memory,
/// spilling older runs to disk.
pub struct CachedRunSeenTracker {
    bloom: Bloom<usize>,

    buffer: Vec<usize>,
    buffer_sorted: bool,
    buffer_limit: usize,

    cached_runs: Vec<Vec<usize>>, // newest last
    max_cached_runs: usize,

    disk_runs: Vec<RunFile>, // oldest first on disk
    run_dir: PathBuf,
    run_counter: usize,
    scratch: Vec<u8>,

    total_seen_count: usize,
}

impl CachedRunSeenTracker {
    pub fn with_path(
        path: &str,
        bloom_capacity: usize,
        buffer_limit: usize,
        max_cached_runs: usize,
    ) -> Self {
        let bloom_capacity = bloom_capacity.max(1_000_000);
        let bloom = Bloom::new_for_fp_rate(bloom_capacity, 0.01);
        let run_dir = PathBuf::from(path);
        if run_dir.exists() {
            let _ = fs::remove_dir_all(&run_dir);
        }
        let _ = fs::create_dir_all(&run_dir);
        Self {
            bloom,
            buffer: Vec::new(),
            buffer_sorted: false,
            buffer_limit,
            cached_runs: Vec::new(),
            max_cached_runs: max_cached_runs.max(1),
            disk_runs: Vec::new(),
            run_dir,
            run_counter: 0,
            scratch: Vec::new(),
            total_seen_count: 0,
        }
    }

    fn ensure_buffer_sorted(&mut self) {
        if !self.buffer_sorted {
            self.buffer.sort_unstable();
            self.buffer.dedup();
            self.buffer_sorted = true;
        }
    }

    fn build_index(ids: &[usize]) -> Vec<RunIndexEntry> {
        let mut index = Vec::new();
        let mut offset: u64 = 8;
        const BLOCK: usize = 4096;
        for chunk in ids.chunks(BLOCK) {
            if let Some(&first) = chunk.first() {
                index.push(RunIndexEntry {
                    first_key: first,
                    last_key: *chunk.last().unwrap_or(&first),
                    offset,
                    len: chunk.len(),
                });
            }
            offset += (chunk.len() * 8) as u64;
        }
        index
    }

    fn next_run_id(&mut self) -> usize {
        let cur = self.run_counter;
        self.run_counter += 1;
        cur
    }

    fn write_run(&mut self, ids: &[usize]) -> Result<(), FoldError> {
        if ids.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(&self.run_dir).map_err(FoldError::Io)?;
        let run_id = self.next_run_id();
        let path = self.run_dir.join(format!("run_{:08}.bin", run_id));
        let mut file = File::create(&path).map_err(FoldError::Io)?;
        let count = ids.len() as u64;
        file.write_all(&count.to_le_bytes())
            .map_err(FoldError::Io)?;
        for id in ids {
            file.write_all(&(*id as u64).to_le_bytes())
                .map_err(FoldError::Io)?;
        }
        file.flush().map_err(FoldError::Io)?;
        let run = RunFile::new(path, Self::build_index(ids))?;
        self.disk_runs.push(run);
        Ok(())
    }

    fn spill_oldest_cached(&mut self) -> Result<(), FoldError> {
        if self.cached_runs.len() <= self.max_cached_runs {
            return Ok(());
        }
        let oldest = self.cached_runs.remove(0);
        self.write_run(&oldest)?;
        Ok(())
    }

    fn contains_in_disk(&mut self, id: usize) -> Result<bool, FoldError> {
        for run in self.disk_runs.iter_mut().rev() {
            if run.index.is_empty() {
                continue;
            }
            if let Some(pos) = run.index.iter().rposition(|e| e.first_key <= id) {
                let entry = run.index[pos].clone();
                if entry.last_key < id {
                    continue;
                }
                let block = run.load_block(&entry, &mut self.scratch)?;
                if block.binary_search(&id).is_ok() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn contains(&mut self, id: &usize) -> bool {
        if !self.bloom.check(id) {
            return false;
        }
        if !self.buffer_sorted {
            self.ensure_buffer_sorted();
        }
        if self.buffer.binary_search(id).is_ok() {
            return true;
        }
        for run in self.cached_runs.iter().rev() {
            if run.binary_search(id).is_ok() {
                return true;
            }
        }
        self.contains_in_disk(*id).unwrap_or(false)
    }

    pub fn insert(&mut self, id: usize) {
        if self.contains(&id) {
            return;
        }
        self.bloom.set(&id);
        self.buffer.push(id);
        self.buffer_sorted = false;
        self.total_seen_count = self.total_seen_count.saturating_add(1);
        if self.buffer.len() >= self.buffer_limit {
            let _ = self.flush_buffer();
        }
    }

    pub fn insert_batch(&mut self, ids: &[usize]) {
        for &id in ids {
            self.insert(id);
        }
    }

    fn flush_buffer(&mut self) -> Result<(), FoldError> {
        self.ensure_buffer_sorted();
        if self.buffer.is_empty() {
            return Ok(());
        }
        let mut to_flush = Vec::new();
        std::mem::swap(&mut to_flush, &mut self.buffer);
        self.buffer_sorted = false;
        self.cached_runs.push(to_flush);
        while self.cached_runs.len() > self.max_cached_runs {
            self.spill_oldest_cached()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), FoldError> {
        self.flush_buffer()?;
        // Optional: spill everything to disk
        while !self.cached_runs.is_empty() {
            let mut run = self.cached_runs.remove(0);
            run.sort_unstable();
            run.dedup();
            self.write_run(&run)?;
        }
        Ok(())
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
    fn cached_runs_basic() {
        let mut tracker =
            CachedRunSeenTracker::with_path("./fold_state/cached_runs", 1000, 16, 2);
        tracker.insert(1);
        tracker.insert(2);
        assert!(tracker.contains(&1));
        tracker.flush().unwrap();
        assert!(tracker.contains(&2));
    }

    #[test]
    fn cached_runs_spill() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("cached_runs");
        let mut tracker = CachedRunSeenTracker::with_path(path.to_str().unwrap(), 1000, 4, 1);
        for i in 0..10 {
            tracker.insert(i);
        }
        tracker.flush().unwrap();
        for i in 0..10 {
            assert!(tracker.contains(&i));
        }
    }
}
