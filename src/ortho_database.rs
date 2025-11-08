use crate::ortho::Ortho;
use crate::error::FoldError;
use std::collections::HashMap;

pub trait OrthoDatabaseLike {
    fn upsert(&mut self, orthos: Vec<Ortho>) -> Result<Vec<Ortho>, FoldError>;
    fn get(&mut self, key: &usize) -> Result<Option<Ortho>, FoldError>;
    fn get_by_dims(&mut self, dims: &[usize]) -> Result<Option<Ortho>, FoldError>;
    fn get_optimal(&mut self) -> Result<Option<Ortho>, FoldError>;
    fn all_orthos(&mut self) -> Result<Vec<Ortho>, FoldError>;
    fn insert_or_update(&mut self, ortho: Ortho) -> Result<(), FoldError>;
    fn remove_by_id(&mut self, id: &usize) -> Result<(), FoldError>;
    fn len(&mut self) -> Result<usize, FoldError>;
}

pub struct InMemoryOrthoDatabase {
    pub map: HashMap<usize, Ortho>,
}

impl InMemoryOrthoDatabase {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
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
        let ortho = Ortho::new();
        let key = ortho.id();
        let new_orthos = db.upsert(vec![ortho.clone()]).expect("upsert should succeed");
        assert_eq!(new_orthos.len(), 1);
        let fetched = db.get(&key).expect("get should succeed");
        assert_eq!(fetched, Some(ortho));
    }

    #[test]
    fn test_upsert_duplicates() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho = Ortho::new();
        let first = db.upsert(vec![ortho.clone()]).expect("first upsert should succeed");
        assert_eq!(first.len(), 1);
        let second = db.upsert(vec![ortho.clone()]).expect("second upsert should succeed");
        assert_eq!(second.len(), 0); // Already seen
    }

    #[test]
    fn test_get_by_dims() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho = Ortho::new();
        let dims = ortho.dims().clone();
        db.upsert(vec![ortho.clone()]).expect("upsert should succeed");
        let found = db.get_by_dims(&dims).expect("get_by_dims should succeed");
        assert_eq!(found, Some(ortho));
    }

    #[test]
    fn test_get_optimal() {
        let mut db = InMemoryOrthoDatabase::new();
        // Start with [2,2]
        let ortho = Ortho::new();
        // Fill [2,2] with 1,2,3
        let ortho = ortho.add(1)[0].add(2)[0].add(3)[0].clone();
        // Now add(4) triggers expansion, producing [3,2] and [2,2,2]
        let expansions = ortho.add(4);
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
        let ortho1 = Ortho::new();
        let ortho2 = Ortho::new();
        db.upsert(vec![ortho1.clone(), ortho2.clone()]).expect("upsert should succeed");
        let orthos = db.all_orthos().expect("all_orthos should succeed");
        assert_eq!(orthos, vec![ortho1]);
    }

    #[test]
    fn test_insert_or_update_and_remove_by_id() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho = Ortho::new();
        db.insert_or_update(ortho.clone()).expect("insert_or_update should succeed");
        let fetched = db.get(&ortho.id()).expect("get should succeed");
        assert_eq!(fetched, Some(ortho.clone()));
        let ortho2 = ortho.add(0)[0].clone();
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
        let ortho_v1 = Ortho::new();
        let ortho_v2 = Ortho::new();
        // Both orthos are identical now (no version field), so they have the same ID
        assert_eq!(ortho_v1.id(), ortho_v2.id());
        db.upsert(vec![ortho_v1.clone()]).expect("upsert should succeed");
        db.upsert(vec![ortho_v2.clone()]).expect("upsert should succeed");
        let fetched_v1 = db.get(&ortho_v1.id()).expect("get should succeed");
        let fetched_v2 = db.get(&ortho_v2.id()).expect("get should succeed");
        assert_eq!(fetched_v1, Some(ortho_v1));
        assert_eq!(fetched_v2, Some(ortho_v2));
    }
}
