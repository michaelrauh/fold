use crate::ortho::Ortho;
use crate::error::FoldError;
use std::collections::HashMap;
use std::path::Path;
use bincode;

pub trait OrthoDatabaseLike {
    fn upsert(&mut self, orthos: Vec<Ortho>) -> Result<Vec<Ortho>, FoldError>;
    fn get(&mut self, key: &usize) -> Result<Option<Ortho>, FoldError>;
    fn get_by_dims(&mut self, dims: &[usize]) -> Result<Option<Ortho>, FoldError>;
    fn get_optimal(&mut self) -> Result<Option<Ortho>, FoldError>;
    fn all_versions(&mut self) -> Result<Vec<usize>, FoldError>;
    fn all_orthos(&mut self) -> Result<Vec<Ortho>, FoldError>;
    fn insert_or_update(&mut self, ortho: Ortho) -> Result<(), FoldError>;
    fn remove_by_id(&mut self, id: &usize) -> Result<(), FoldError>;
    fn len(&mut self) -> Result<usize, FoldError>;
    fn sample_version(&mut self, version: usize) -> Result<Option<Ortho>, FoldError>;
    fn version_counts(&mut self) -> Result<Vec<(usize, usize)>, FoldError>; // (version, count)
}

pub struct InMemoryOrthoDatabase {
    pub map: HashMap<usize, Ortho>,
}

impl InMemoryOrthoDatabase {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn save_to_path(&self, path: &Path) -> Result<(), FoldError> {
        let bytes = bincode::encode_to_vec(&self.map, bincode::config::standard())?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn load_from_path(&mut self, path: &Path) -> Result<(), FoldError> {
        let data = std::fs::read(path)?;
        let (map, _): (HashMap<usize, Ortho>, usize) = bincode::decode_from_slice(&data, bincode::config::standard())?;
        self.map = map;
        Ok(())
    }
}

impl OrthoDatabaseLike for InMemoryOrthoDatabase {
    fn upsert(&mut self, orthos: Vec<Ortho>) -> Result<Vec<Ortho>, FoldError> {
        let mut new_orthos = Vec::new();
        for ortho in orthos {
            let key = ortho.id();
            let prev = self.map.insert(key, ortho.clone());
            if prev.is_none() {
                new_orthos.push(ortho);
            }
        }
        Ok(new_orthos)
    }
    fn get(&mut self, key: &usize) -> Result<Option<Ortho>, FoldError> {
        Ok(self.map.get(key).cloned())
    }
    fn get_by_dims(&mut self, dims: &[usize]) -> Result<Option<Ortho>, FoldError> {
        Ok(self.map.values().find(|o| o.dims() == dims).cloned())
    }

    fn get_optimal(&mut self) -> Result<Option<Ortho>, FoldError> {
        Ok(self.map.values()
            .max_by_key(|o| o.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>())
            .cloned())
    }
    fn all_versions(&mut self) -> Result<Vec<usize>, FoldError> {
        use std::collections::HashSet;
        let versions: HashSet<usize> = self.map.values().map(|o| o.version()).collect();
        let mut versions_vec: Vec<usize> = versions.into_iter().collect();
        versions_vec.sort_unstable();
        Ok(versions_vec)
    }
    fn all_orthos(&mut self) -> Result<Vec<Ortho>, FoldError> {
        Ok(self.map.values().cloned().collect())
    }

    fn insert_or_update(&mut self, ortho: Ortho) -> Result<(), FoldError> {
        self.map.insert(ortho.id(), ortho);
        Ok(())
    }
    fn remove_by_id(&mut self, id: &usize) -> Result<(), FoldError> {
        self.map.remove(id);
        Ok(())
    }
    fn len(&mut self) -> Result<usize, FoldError> {
        Ok(self.map.len())
    }
    fn sample_version(&mut self, version: usize) -> Result<Option<Ortho>, FoldError> {
        Ok(self.map.values().find(|o| o.version() == version).cloned())
    }
    fn version_counts(&mut self) -> Result<Vec<(usize, usize)>, FoldError> {
        use std::collections::HashMap as StdHashMap;
        let mut counts: StdHashMap<usize, usize> = StdHashMap::new();
        for o in self.map.values() { *counts.entry(o.version()).or_insert(0) += 1; }
        let mut v: Vec<(usize, usize)> = counts.into_iter().collect();
        v.sort_by_key(|(ver, _)| *ver);
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;

    #[test]
    fn test_new() {
        let db = InMemoryOrthoDatabase::new();
        assert_eq!(db.map.len(), 0);
    }

    #[test]
    fn test_upsert_and_get() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho = Ortho::new(1);
        let key = ortho.id();
        let new_orthos = db.upsert(vec![ortho.clone()]).expect("upsert should succeed");
        assert_eq!(new_orthos.len(), 1);
        let fetched = db.get(&key).expect("get should succeed");
        assert_eq!(fetched, Some(ortho));
    }

    #[test]
    fn test_upsert_duplicates() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho = Ortho::new(1);
        let first = db.upsert(vec![ortho.clone()]).expect("first upsert should succeed");
        assert_eq!(first.len(), 1);
        let second = db.upsert(vec![ortho.clone()]).expect("second upsert should succeed");
        assert_eq!(second.len(), 0); // Already seen
    }

    #[test]
    fn test_get_by_dims() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho = Ortho::new(1);
        let dims = ortho.dims().clone();
        db.upsert(vec![ortho.clone()]).expect("upsert should succeed");
        let found = db.get_by_dims(&dims).expect("get_by_dims should succeed");
        assert_eq!(found, Some(ortho));
    }

    #[test]
    fn test_get_optimal() {
        let mut db = InMemoryOrthoDatabase::new();
        // Start with [2,2]
        let ortho = Ortho::new(1);
        // Fill [2,2] with 1,2,3
        let ortho = ortho.add(1, 1)[0].add(2, 1)[0].add(3, 1)[0].clone();
        // Now add(4, 1) triggers expansion, producing [3,2] and [2,2,2]
        let expansions = ortho.add(4, 1);
        let ortho_3_2 = expansions
            .iter()
            .find(|o| o.dims() == &vec![3, 2])
            .unwrap()
            .clone();
        let ortho_2_2_2 = expansions
            .iter()
            .find(|o| o.dims() == &vec![2, 2, 2])
            .unwrap()
            .clone();
        db.upsert(vec![ortho.clone(), ortho_3_2.clone(), ortho_2_2_2.clone()]).expect("upsert should succeed");
        let optimal = db.get_optimal().expect("get_optimal should succeed");
        // [2,2] -> 1, [3,2] -> 2, [2,2,2] -> 1
        assert_eq!(optimal, Some(ortho_3_2));
    }

    #[test]
    fn test_all_versions_and_all_orthos() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        db.upsert(vec![ortho1.clone(), ortho2.clone()]).expect("upsert should succeed");
        let mut versions = db.all_versions().expect("all_versions should succeed");
        versions.sort();
        assert_eq!(versions, vec![1, 2]);
        let mut orthos = db.all_orthos().expect("all_orthos should succeed");
        orthos.sort_by_key(|o| o.version());
        assert_eq!(orthos, vec![ortho1, ortho2]);
    }

    #[test]
    fn test_insert_or_update_and_remove_by_id() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho = Ortho::new(5);
        db.insert_or_update(ortho.clone()).expect("insert_or_update should succeed");
        let fetched = db.get(&ortho.id()).expect("get should succeed");
        assert_eq!(fetched, Some(ortho.clone()));
        let ortho2 = ortho.add(0, 10)[0].clone();
        db.insert_or_update(ortho2.clone()).expect("insert_or_update should succeed");
        let fetched2 = db.get(&ortho2.id()).expect("get should succeed");
        assert_eq!(fetched2, Some(ortho2.clone()));
        db.remove_by_id(&ortho2.id()).expect("remove_by_id should succeed");
        let fetched3 = db.get(&ortho2.id()).expect("get should succeed");
        assert_eq!(fetched3, None);
    }

    #[test]
    fn test_version_handling() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho_v1 = Ortho::new(1);
        let ortho_v2 = Ortho::new(2);
        db.upsert(vec![ortho_v1.clone()]).expect("upsert should succeed");
        db.upsert(vec![ortho_v2.clone()]).expect("upsert should succeed");
        let mut versions = db.all_versions().expect("all_versions should succeed");
        versions.sort();
        assert_eq!(versions, vec![1, 2]);
        let fetched_v1 = db.get(&ortho_v1.id()).expect("get should succeed");
        let fetched_v2 = db.get(&ortho_v2.id()).expect("get should succeed");
        assert_eq!(fetched_v1, Some(ortho_v1));
        assert_eq!(fetched_v2, Some(ortho_v2));
    }
}
