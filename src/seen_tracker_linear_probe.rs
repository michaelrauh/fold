use crate::FoldError;
use nohash_hasher::BuildNoHashHasher;
use std::fs::OpenOptions;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufReader, BufWriter, Read, Write};

/// Simple linear-probing hash table for usize keys with on-disk assisted resize
/// to avoid holding old+new tables simultaneously. Uses a nohash hasher for speed.
pub struct LinearProbeDiskResizeTracker {
    table: Vec<Option<usize>>,
    len: usize,
    max_load: f64,
    hasher: BuildNoHashHasher<usize>,
}

impl LinearProbeDiskResizeTracker {
    pub fn new(initial_capacity: usize, max_load: f64) -> Self {
        let cap = initial_capacity.next_power_of_two().max(16_384);
        Self {
            table: vec![None; cap],
            len: 0,
            max_load,
            hasher: BuildNoHashHasher::default(),
        }
    }

    pub fn contains(&self, key: &usize) -> bool {
        self.find_slot(*key).1.is_some()
    }

    pub fn insert(&mut self, key: usize) {
        if self.contains(&key) {
            return;
        }
        if self.load_factor() >= self.max_load {
            let _ = self.resize_with_disk();
        }
        let (idx, _) = self.find_slot(key);
        self.table[idx] = Some(key);
        self.len = self.len.saturating_add(1);
    }

    pub fn insert_batch(&mut self, keys: &[usize]) {
        for &k in keys {
            self.insert(k);
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    fn load_factor(&self) -> f64 {
        self.len as f64 / self.table.len() as f64
    }

    fn hash(&self, key: usize) -> usize {
        // nohash: identity
        let mut h = self.hasher.build_hasher();
        h.write_usize(key);
        h.finish() as usize
    }

    fn find_slot(&self, key: usize) -> (usize, Option<usize>) {
        let mut idx = self.hash(key) & (self.table.len() - 1);
        loop {
            match self.table[idx] {
                Some(k) if k == key => return (idx, Some(k)),
                None => return (idx, None),
                _ => {
                    idx = (idx + 1) & (self.table.len() - 1);
                }
            }
        }
    }

    fn resize_with_disk(&mut self) -> Result<(), FoldError> {
        // Spill all current keys to a temp file, drop the old table, allocate new, reinsert.
        let tmp = tempfile::NamedTempFile::new().map_err(FoldError::Io)?;
        {
            let mut w = BufWriter::new(&tmp);
            for slot in self.table.iter().flatten() {
                w.write_all(&slot.to_le_bytes()).map_err(FoldError::Io)?;
            }
            w.flush().map_err(FoldError::Io)?;
        }
        let new_cap = (self.table.len() * 2).max(32_768);
        self.table = vec![None; new_cap];
        self.len = 0;

        let file = OpenOptions::new()
            .read(true)
            .open(tmp.path())
            .map_err(FoldError::Io)?;
        let mut reader = BufReader::new(file);
        let mut buf = [0u8; 8];
        loop {
            match reader.read_exact(&mut buf) {
                Ok(()) => {
                    let key = usize::from_le_bytes(buf);
                    self.insert(key);
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(FoldError::Io(e)),
            }
        }
        Ok(())
    }
}
