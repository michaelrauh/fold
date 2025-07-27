use crate::ortho::Ortho;
use std::collections::HashMap;

pub struct OrthoDatabase {
    pub map: HashMap<usize, Ortho>,
}

impl OrthoDatabase {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn upsert(&mut self, orthos: Vec<Ortho>) -> Vec<Ortho> {
        let mut new_orthos = Vec::new();
        for ortho in orthos {
            let key = ortho.id();
            let prev = self.map.insert(key, ortho.clone());
            if prev.is_none() {
                new_orthos.push(ortho);
            }
        }
        new_orthos
    }

    pub fn get(&self, key: &usize) -> Option<Ortho> {
        self.map.get(key).cloned()
    }

    pub fn get_by_dims(&self, dims: &[usize]) -> Option<Ortho> {
        self.map.values().find(|o| o.dims() == dims).cloned()
    }

    pub fn get_optimal(&self) -> Option<Ortho> {
        self.map.values()
            .max_by_key(|o| {
                o.dims()
                    .iter()
                    .map(|x| x.saturating_sub(1))
                    .product::<usize>()
            })
            .cloned()
    }

    pub fn all_versions(&self) -> Vec<usize> {
        use std::collections::HashSet;
        let versions: HashSet<usize> = self.map.values().map(|o| o.version()).collect();
        let mut versions_vec: Vec<usize> = versions.into_iter().collect();
        versions_vec.sort_unstable();
        versions_vec
    }

    pub fn log_map_length_periodically(self) {
        std::thread::spawn(move || {
            loop {
                let length = self.map.len();
                println!("[map length: {}", length);
                std::thread::sleep(std::time::Duration::from_millis(750));
            }
        });
    }

    pub fn all_orthos(&self) -> Vec<Ortho> {
        self.map.values().cloned().collect()
    }

    pub fn insert_or_update(&mut self, ortho: Ortho) {
        self.map.insert(ortho.id(), ortho);
    }

    pub fn remove_by_id(&mut self, id: &usize) {
        self.map.remove(id);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;

    #[test]
    fn test_new() {
        let db = OrthoDatabase::new();
        assert_eq!(db.map.len(), 0);
    }

    #[test]
    fn test_upsert_and_get() {
        let mut db = OrthoDatabase::new();
        let ortho = Ortho::new(1);
        let key = ortho.id();
        let new_orthos = db.upsert(vec![ortho.clone()]);
        assert_eq!(new_orthos.len(), 1);
        let fetched = db.get(&key);
        assert_eq!(fetched, Some(ortho));
    }

    #[test]
    fn test_upsert_duplicates() {
        let mut db = OrthoDatabase::new();
        let ortho = Ortho::new(1);
        let first = db.upsert(vec![ortho.clone()]);
        assert_eq!(first.len(), 1);
        let second = db.upsert(vec![ortho.clone()]);
        assert_eq!(second.len(), 0); // Already seen
    }

    #[test]
    fn test_get_by_dims() {
        let mut db = OrthoDatabase::new();
        let ortho = Ortho::new(1);
        let dims = ortho.dims().clone();
        db.upsert(vec![ortho.clone()]);
        let found = db.get_by_dims(&dims);
        assert_eq!(found, Some(ortho));
    }

    #[test]
    fn test_get_optimal() {
        let mut db = OrthoDatabase::new();
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
        db.upsert(vec![ortho.clone(), ortho_3_2.clone(), ortho_2_2_2.clone()]);
        let optimal = db.get_optimal();
        // [2,2] -> 1, [3,2] -> 2, [2,2,2] -> 1
        assert_eq!(optimal, Some(ortho_3_2));
    }

    #[test]
    fn test_all_versions_and_all_orthos() {
        let mut db = OrthoDatabase::new();
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        db.upsert(vec![ortho1.clone(), ortho2.clone()]);
        let mut versions = db.all_versions();
        versions.sort();
        assert_eq!(versions, vec![1, 2]);
        let mut orthos = db.all_orthos();
        orthos.sort_by_key(|o| o.version());
        assert_eq!(orthos, vec![ortho1, ortho2]);
    }

    #[test]
    fn test_insert_or_update_and_remove_by_id() {
        let mut db = OrthoDatabase::new();
        let ortho = Ortho::new(5);
        db.insert_or_update(ortho.clone());
        let fetched = db.get(&ortho.id());
        assert_eq!(fetched, Some(ortho.clone()));
        let ortho2 = ortho.add(0, 10)[0].clone();
        db.insert_or_update(ortho2.clone());
        let fetched2 = db.get(&ortho2.id());
        assert_eq!(fetched2, Some(ortho2.clone()));
        db.remove_by_id(&ortho2.id());
        let fetched3 = db.get(&ortho2.id());
        assert_eq!(fetched3, None);
    }

    #[test]
    fn test_version_handling() {
        let mut db = OrthoDatabase::new();
        let ortho_v1 = Ortho::new(1);
        let ortho_v2 = Ortho::new(2);
        db.upsert(vec![ortho_v1.clone()]);
        db.upsert(vec![ortho_v2.clone()]);
        let mut versions = db.all_versions();
        versions.sort();
        assert_eq!(versions, vec![1, 2]);
        let fetched_v1 = db.get(&ortho_v1.id());
        let fetched_v2 = db.get(&ortho_v2.id());
        assert_eq!(fetched_v1, Some(ortho_v1));
        assert_eq!(fetched_v2, Some(ortho_v2));
    }
}
