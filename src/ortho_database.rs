use crate::ortho::Ortho;
use itertools::Itertools;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct OrthoDatabase {
    pub map: Arc<Mutex<HashMap<usize, Ortho>>>,
}

impl OrthoDatabase {
    pub fn new() -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    pub async fn upsert(&self, orthos: Vec<Ortho>) -> Vec<Ortho> {
        let mut new_orthos = Vec::new();
        let mut map = self.map.lock().await;
        for ortho in orthos {
            let key = ortho.id();
            let prev = map.insert(key, ortho.clone());
            if prev.is_none() {
                new_orthos.push(ortho);
            }
        }
        new_orthos
    }
    pub async fn get(&self, key: &usize) -> Option<Ortho> {
        let map = self.map.lock().await;
        map.get(key).cloned()
    }
    pub async fn get_by_dims(&self, dims: &[usize]) -> Option<Ortho> {
        let map = self.map.lock().await;
        map.values().find(|o| o.dims() == dims).cloned()
    }
    pub async fn get_optimal(&self) -> Option<Ortho> {
        let map = self.map.lock().await;
        map.values()
            .max_by_key(|o| {
                o.dims()
                    .iter()
                    .map(|x| x.saturating_sub(1))
                    .product::<usize>()
            })
            .cloned()
    }

    pub async fn all_versions(&self) -> Vec<usize> {
        let map = self.map.lock().await;
        let versions: Vec<usize> = map.values().map(|o| o.version()).sorted().collect();
        eprintln!("[OrthoDatabase] all_versions: {:?}", versions);
        versions
    }

    pub async fn all_orthos(&self) -> Vec<Ortho> {
        let map = self.map.lock().await;
        map.values().cloned().collect()
    }

    pub async fn insert_or_update(&self, ortho: Ortho) {
        let mut map = self.map.lock().await;
        eprintln!("[OrthoDatabase] insert_or_update: inserting id={} version={}", ortho.id(), ortho.version());
        map.insert(ortho.id(), ortho);
        eprintln!("[OrthoDatabase] after insert_or_update, map keys: {:?}", map.keys().collect::<Vec<_>>());
    }

    pub async fn remove_by_id(&self, id: &usize) {
        let mut map = self.map.lock().await;
        eprintln!("[OrthoDatabase] remove_by_id: removing id={}", id);
        map.remove(id);
        eprintln!("[OrthoDatabase] after remove_by_id, map keys: {:?}", map.keys().collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;
    use tokio::runtime::Runtime;

    #[test]
    fn test_new() {
        let db = OrthoDatabase::new();
        // Check that map and seen are empty
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            assert_eq!(db.map.lock().await.len(), 0);
        });
    }

    #[test]
    fn test_upsert_and_get() {
        let db = OrthoDatabase::new();
        let ortho = Ortho::new(1);
        let key = ortho.id();
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let new_orthos = db.upsert(vec![ortho.clone()]).await;
            assert_eq!(new_orthos.len(), 1);
            let fetched = db.get(&key).await;
            assert_eq!(fetched, Some(ortho));
        });
    }

    #[test]
    fn test_upsert_duplicates() {
        let db = OrthoDatabase::new();
        let ortho = Ortho::new(1);
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let first = db.upsert(vec![ortho.clone()]).await;
            assert_eq!(first.len(), 1);
            let second = db.upsert(vec![ortho.clone()]).await;
            assert_eq!(second.len(), 0); // Already seen
        });
    }

    #[test]
    fn test_get_by_dims() {
        let db = OrthoDatabase::new();
        let ortho = Ortho::new(1);
        let dims = ortho.dims().clone();
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            db.upsert(vec![ortho.clone()]).await;
            let found = db.get_by_dims(&dims).await;
            assert_eq!(found, Some(ortho));
        });
    }

    #[test]
    fn test_get_optimal() {
        let db = OrthoDatabase::new();
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
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            db.upsert(vec![ortho.clone(), ortho_3_2.clone(), ortho_2_2_2.clone()])
                .await;
            let optimal = db.get_optimal().await;
            // [2,2] -> 1, [3,2] -> 2, [2,2,2] -> 1
            assert_eq!(optimal, Some(ortho_3_2));
        });
    }

    #[tokio::test]
    async fn test_all_versions_and_all_orthos() {
        let db = OrthoDatabase::new();
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        db.upsert(vec![ortho1.clone(), ortho2.clone()]).await;
        let mut versions = db.all_versions().await;
        versions.sort();
        assert_eq!(versions, vec![1, 2]);
        let mut orthos = db.all_orthos().await;
        orthos.sort_by_key(|o| o.version());
        assert_eq!(orthos, vec![ortho1, ortho2]);
    }

    #[tokio::test]
    async fn test_insert_or_update_and_remove_by_id() {
        let db = OrthoDatabase::new();
        let ortho = Ortho::new(5);
        db.insert_or_update(ortho.clone()).await;
        let fetched = db.get(&ortho.id()).await;
        assert_eq!(fetched, Some(ortho.clone()));
        let ortho2 = ortho.add(0, 10)[0].clone();
        db.insert_or_update(ortho2.clone()).await;
        let fetched2 = db.get(&ortho2.id()).await;
        assert_eq!(fetched2, Some(ortho2.clone()));
        db.remove_by_id(&ortho2.id()).await;
        let fetched3 = db.get(&ortho2.id()).await;
        assert_eq!(fetched3, None);
    }

    #[test]
    fn test_version_handling() {
        let db = OrthoDatabase::new();
        let ortho_v1 = Ortho::new(1);
        let ortho_v2 = Ortho::new(2);
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            db.upsert(vec![ortho_v1.clone()]).await;
            db.upsert(vec![ortho_v2.clone()]).await;
            let mut versions = db.all_versions().await;
            versions.sort();
            assert_eq!(versions, vec![1, 2]);
            let fetched_v1 = db.get(&ortho_v1.id()).await;
            let fetched_v2 = db.get(&ortho_v2.id()).await;
            assert_eq!(fetched_v1, Some(ortho_v1));
            assert_eq!(fetched_v2, Some(ortho_v2));
        });
    }
}
