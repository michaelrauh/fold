use crate::ortho::Ortho;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct OrthoDatabase {
    pub map: Arc<Mutex<HashMap<usize, Ortho>>>,
    pub seen: Arc<Mutex<HashSet<usize>>>,
}

impl OrthoDatabase {
    pub fn new() -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
            seen: Arc::new(Mutex::new(HashSet::new())),
        }
    }
    pub async fn upsert(&self, orthos: Vec<Ortho>) -> Vec<Ortho> {
        let mut new_orthos = Vec::new();
        let mut map = self.map.lock().await;
        let mut seen = self.seen.lock().await;
        for ortho in orthos {
            let key = ortho.id();
            let is_new = seen.insert(key);
            map.insert(key, ortho.clone());
            if is_new {
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
            assert_eq!(db.seen.lock().await.len(), 0);
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
}
