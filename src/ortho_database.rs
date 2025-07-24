use std::collections::{HashSet, HashMap};
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::ortho::Ortho;

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
