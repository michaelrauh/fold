use crate::ortho::Ortho;
use std::collections::HashMap;
use postgres::{Client, NoTls};
use bincode::{encode_to_vec, decode_from_slice, config::standard};
use std::env;
use tracing::instrument;

pub trait OrthoDatabaseLike {
    fn upsert(&mut self, orthos: Vec<Ortho>) -> Vec<Ortho>;
    fn get(&mut self, key: &usize) -> Option<Ortho>;
    fn get_by_dims(&mut self, dims: &[usize]) -> Option<Ortho>;
    fn get_optimal(&mut self) -> Option<Ortho>;
    fn all_versions(&mut self) -> Vec<usize>;
    fn all_orthos(&mut self) -> Vec<Ortho>;
    fn insert_or_update(&mut self, ortho: Ortho);
    fn remove_by_id(&mut self, id: &usize);
    fn len(&mut self) -> usize;
    fn sample_version(&mut self, version: usize) -> Option<Ortho>;
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
    fn upsert(&mut self, orthos: Vec<Ortho>) -> Vec<Ortho> {
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
    fn get(&mut self, key: &usize) -> Option<Ortho> {
        self.map.get(key).cloned()
    }
    fn get_by_dims(&mut self, dims: &[usize]) -> Option<Ortho> {
        self.map.values().find(|o| o.dims() == dims).cloned()
    }

    fn get_optimal(&mut self) -> Option<Ortho> {
        self.map.values()
            .max_by_key(|o| o.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>())
            .cloned()
    }
    fn all_versions(&mut self) -> Vec<usize> {
        use std::collections::HashSet;
        let versions: HashSet<usize> = self.map.values().map(|o| o.version()).collect();
        let mut versions_vec: Vec<usize> = versions.into_iter().collect();
        versions_vec.sort_unstable();
        versions_vec
    }
    fn all_orthos(&mut self) -> Vec<Ortho> {
        self.map.values().cloned().collect()
    }

    fn insert_or_update(&mut self, ortho: Ortho) {
        self.map.insert(ortho.id(), ortho);
    }
    fn remove_by_id(&mut self, id: &usize) {
        self.map.remove(id);
    }
    fn len(&mut self) -> usize {
        let l = self.map.len();
        l
    }
    fn sample_version(&mut self, version: usize) -> Option<Ortho> {
        self.map.values().find(|o| o.version() == version).cloned()
    }
}

pub struct PostgresOrthoDatabase {
    pub client: Client,
}

impl PostgresOrthoDatabase {
    pub fn new() -> Self {
        let conn_str = env::var("FOLD_PG_URL").expect("FOLD_PG_URL environment variable must be set for PostgresOrthoDatabase");
        let mut client = Client::connect(&conn_str, NoTls).expect("Failed to connect to Postgres");
        client.batch_execute("
            CREATE TABLE IF NOT EXISTS orthos (
                id BIGINT PRIMARY KEY,
                version BIGINT NOT NULL,
                dims BYTEA NOT NULL,
                data BYTEA NOT NULL
            );
        ").unwrap();
        Self { client }
    }
}

impl OrthoDatabaseLike for PostgresOrthoDatabase {
    #[instrument(skip_all)]
    fn upsert(&mut self, orthos: Vec<Ortho>) -> Vec<Ortho> {
        if orthos.is_empty() {
            return Vec::new();
        }
        let mut params: Vec<Box<dyn postgres::types::ToSql + Sync>> = Vec::new();
        let mut values = Vec::new();
        for (i, ortho) in orthos.iter().enumerate() {
            values.push(format!("(${}, ${}, ${}, ${})", i*4+1, i*4+2, i*4+3, i*4+4));
            params.push(Box::new(ortho.id() as i64));
            params.push(Box::new(ortho.version() as i64));
            params.push(Box::new(encode_to_vec(&ortho.dims(), standard()).unwrap()));
            params.push(Box::new(encode_to_vec(&ortho, standard()).unwrap()));
        }
        let param_refs: Vec<&(dyn postgres::types::ToSql + Sync)> = params.iter().map(|b| &**b).collect();
        let sql = format!(
            "INSERT INTO orthos (id, version, dims, data) VALUES {} ON CONFLICT (id) DO NOTHING RETURNING data",
            values.join(", ")
        );
        self.client.query(&sql, &param_refs).unwrap()
            .into_iter()
            .filter_map(|row| {
                let data: Vec<u8> = row.get(0);
                decode_from_slice::<Ortho, _>(&data, standard()).ok().map(|(o, _)| o)
            })
            .collect()
    }
    #[instrument(skip_all)]
    fn get(&mut self, key: &usize) -> Option<Ortho> {
        let id = *key as i64;
        let row = self.client.query_opt("SELECT data FROM orthos WHERE id = $1", &[&id]).unwrap();
        row.and_then(|r| {
            let data: Vec<u8> = r.get(0);
            decode_from_slice::<Ortho, _>(&data, standard()).ok().map(|(o, _)| o)
        })
    }
    #[instrument(skip_all)]
    fn get_by_dims(&mut self, dims: &[usize]) -> Option<Ortho> {
        let dims_bin = encode_to_vec(dims, standard()).unwrap();
        let row = self.client.query_opt("SELECT data FROM orthos WHERE dims = $1", &[&dims_bin]).unwrap();
        row.and_then(|r| {
            let data: Vec<u8> = r.get(0);
            decode_from_slice::<Ortho, _>(&data, standard()).ok().map(|(o, _)| o)
        })
    }
    #[instrument(skip(self))]
    fn get_optimal(&mut self) -> Option<Ortho> {
        // Step 1: Get all dims
        let rows = self.client.query("SELECT DISTINCT dims FROM orthos", &[]).unwrap();
        let dims_list: Vec<Vec<usize>> = rows.into_iter().filter_map(|r| {
            let dims_bin: Vec<u8> = r.get(0);
            decode_from_slice::<Vec<usize>, _>(&dims_bin, standard()).ok().map(|(d, _)| d)
        }).collect();
        // Step 2: Find optimal dims
        let optimal_dims = dims_list.into_iter().max_by_key(|dims| {
            dims.iter().map(|x| x.saturating_sub(1)).product::<usize>()
        });
        // Step 3: Query for one ortho with those dims
        if let Some(dims) = optimal_dims {
            let dims_bin = encode_to_vec(&dims, standard()).unwrap();
            let row = self.client.query_opt("SELECT data FROM orthos WHERE dims = $1 LIMIT 1", &[&dims_bin]).unwrap();
            row.and_then(|r| {
                let data: Vec<u8> = r.get(0);
                decode_from_slice::<Ortho, _>(&data, standard()).ok().map(|(o, _)| o)
            })
        } else {
            None
        }
    }
    #[instrument(skip_all)]
    fn all_versions(&mut self) -> Vec<usize> {
        let rows = self.client.query("SELECT DISTINCT version FROM orthos", &[]).unwrap();
        let mut versions: Vec<usize> = rows.into_iter().map(|r| {
            let version: i64 = r.get(0);
            version as usize
        }).collect();
        versions.sort_unstable();
        versions
    }
    #[instrument(skip_all)]
    fn all_orthos(&mut self) -> Vec<Ortho> {
        let rows = self.client.query("SELECT data FROM orthos", &[]).unwrap();
        rows.into_iter().filter_map(|r| {
            let data: Vec<u8> = r.get(0);
            decode_from_slice(&data, standard()).ok().map(|(o, _)| o)
        }).collect()
    }
    #[instrument(skip(self, ortho))]
    fn insert_or_update(&mut self, ortho: Ortho) {
        let id = ortho.id() as i64;
        let version = ortho.version() as i64;
        let dims = encode_to_vec(&ortho.dims(), standard()).unwrap();
        let data = encode_to_vec(&ortho, standard()).unwrap();
        self.client.execute(
            "INSERT INTO orthos (id, version, dims, data) VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET version = EXCLUDED.version",
            &[&id, &version, &dims, &data],
        ).unwrap();
    }
    #[instrument(skip_all)]
    fn remove_by_id(&mut self, id: &usize) {
        let id = *id as i64;
        self.client.execute("DELETE FROM orthos WHERE id = $1", &[&id]).unwrap();
    }
    #[instrument(skip_all)]
    fn len(&mut self) -> usize {
        let row = self.client.query_one("SELECT COUNT(*) FROM orthos", &[]).unwrap();
        let count: i64 = row.get(0);
        let l = count as usize;
        l
    }
    #[instrument(skip_all)]
    fn sample_version(&mut self, _version: usize) -> Option<Ortho> {
        // Return the first Ortho with the given version, or None
        let version = _version as i64;
        let row = self.client.query_opt("SELECT data FROM orthos WHERE version = $1 LIMIT 1", &[&version]).unwrap();
        row.and_then(|r| {
            let data: Vec<u8> = r.get(0);
            decode_from_slice::<Ortho, _>(&data, standard()).ok().map(|(o, _)| o)
        })
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
        let new_orthos = db.upsert(vec![ortho.clone()]);
        assert_eq!(new_orthos.len(), 1);
        let fetched = db.get(&key);
        assert_eq!(fetched, Some(ortho));
    }

    #[test]
    fn test_upsert_duplicates() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho = Ortho::new(1);
        let first = db.upsert(vec![ortho.clone()]);
        assert_eq!(first.len(), 1);
        let second = db.upsert(vec![ortho.clone()]);
        assert_eq!(second.len(), 0); // Already seen
    }

    #[test]
    fn test_get_by_dims() {
        let mut db = InMemoryOrthoDatabase::new();
        let ortho = Ortho::new(1);
        let dims = ortho.dims().clone();
        db.upsert(vec![ortho.clone()]);
        let found = db.get_by_dims(&dims);
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
        db.upsert(vec![ortho.clone(), ortho_3_2.clone(), ortho_2_2_2.clone()]);
        let optimal = db.get_optimal();
        // [2,2] -> 1, [3,2] -> 2, [2,2,2] -> 1
        assert_eq!(optimal, Some(ortho_3_2));
    }

    #[test]
    fn test_all_versions_and_all_orthos() {
        let mut db = InMemoryOrthoDatabase::new();
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
        let mut db = InMemoryOrthoDatabase::new();
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
        let mut db = InMemoryOrthoDatabase::new();
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
