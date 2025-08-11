use crate::ortho::Ortho;
use crate::error::FoldError;
use std::collections::HashMap;
use postgres::{Client, NoTls};
use bincode::{encode_to_vec, decode_from_slice, config::standard};
use std::env;
use tracing::instrument;

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

pub struct PostgresOrthoDatabase {
    pub client: Client,
    follower_id: String,
    upsert_prepared: bool,
}

impl PostgresOrthoDatabase {
    pub fn new() -> Self { Self::new_with_follower_id("default".to_string()) }
    pub fn new_with_follower_id(follower_id: String) -> Self {
        let conn_str = env::var("FOLD_PG_URL").expect("FOLD_PG_URL environment variable must be set for PostgresOrthoDatabase");
        let mut client = Client::connect(&conn_str, NoTls).expect("Failed to connect to Postgres");
        client.batch_execute("
            CREATE TABLE IF NOT EXISTS orthos (
                id BIGINT PRIMARY KEY,
                version BIGINT NOT NULL,
                dims BYTEA NOT NULL,
                data BYTEA NOT NULL,
                claimed_at TIMESTAMPTZ,
                claimed_by TEXT
            );
            ALTER TABLE orthos ADD COLUMN IF NOT EXISTS claimed_at TIMESTAMPTZ;
            ALTER TABLE orthos ADD COLUMN IF NOT EXISTS claimed_by TEXT;
        ").unwrap();
        let _ = client.batch_execute("CREATE INDEX CONCURRENTLY IF NOT EXISTS ix_orthos_ready ON orthos (version, id) WHERE claimed_at IS NULL;");
        Self { client, follower_id, upsert_prepared: false }
    }

    fn ensure_upsert_prepared(&mut self) {
        if self.upsert_prepared { return; }
        let _ = self.client.prepare("INSERT INTO orthos (id, version, dims, data) SELECT * FROM UNNEST($1::bigint[], $2::bigint[], $3::bytea[], $4::bytea[]) ON CONFLICT (id) DO NOTHING RETURNING data");
        self.upsert_prepared = true;
    }

    #[instrument(skip_all)]
    pub fn reap_stale_claims(&mut self) {
        // 45s timeout window
        let _ = self.client.execute(
            "UPDATE orthos SET claimed_at = NULL, claimed_by = NULL WHERE claimed_at < now() - interval '45 seconds'",
            &[]
        );
    }
}

impl OrthoDatabaseLike for PostgresOrthoDatabase {
    #[instrument(skip_all)]
    fn upsert(&mut self, orthos: Vec<Ortho>) -> Result<Vec<Ortho>, FoldError> {
        if orthos.is_empty() { return Ok(Vec::new()); }
        self.ensure_upsert_prepared();
        let mut ids: Vec<i64> = Vec::with_capacity(orthos.len());
        let mut versions: Vec<i64> = Vec::with_capacity(orthos.len());
        let mut dims_arr: Vec<Vec<u8>> = Vec::with_capacity(orthos.len());
        let mut data_arr: Vec<Vec<u8>> = Vec::with_capacity(orthos.len());
        for o in &orthos {
            ids.push(o.id() as i64);
            versions.push(o.version() as i64);
            dims_arr.push(encode_to_vec(&o.dims(), standard())?);
            data_arr.push(encode_to_vec(o, standard())?);
        }
        let rows = self.client.query(
            "INSERT INTO orthos (id, version, dims, data) SELECT * FROM UNNEST($1::bigint[], $2::bigint[], $3::bytea[], $4::bytea[]) ON CONFLICT (id) DO NOTHING RETURNING data",
            &[&ids, &versions, &dims_arr, &data_arr],
        )?;
        let mut inserted = Vec::with_capacity(rows.len());
        for row in rows { let data: Vec<u8> = row.get(0); if let Ok((o,_)) = decode_from_slice::<Ortho,_>(&data, standard()) { inserted.push(o); } }
        Ok(inserted)
    }

    #[instrument(skip_all)]
    fn get(&mut self, key: &usize) -> Result<Option<Ortho>, FoldError> {
        let id = *key as i64;
        let row = self.client.query_opt("SELECT data FROM orthos WHERE id = $1", &[&id])?;
        Ok(row.and_then(|r| {
            let data: Vec<u8> = r.get(0);
            decode_from_slice::<Ortho, _>(&data, standard()).ok().map(|(o, _)| o)
        }))
    }
    #[instrument(skip_all)]
    fn get_by_dims(&mut self, dims: &[usize]) -> Result<Option<Ortho>, FoldError> {
        let dims_bin = encode_to_vec(dims, standard())?;
        let row = self.client.query_opt("SELECT data FROM orthos WHERE dims = $1", &[&dims_bin])?;
        Ok(row.and_then(|r| {
            let data: Vec<u8> = r.get(0);
            decode_from_slice::<Ortho, _>(&data, standard()).ok().map(|(o, _)| o)
        }))
    }
    #[instrument(skip(self))]
    fn get_optimal(&mut self) -> Result<Option<Ortho>, FoldError> {
        // Step 1: Get all dims
        let rows = self.client.query("SELECT DISTINCT dims FROM orthos", &[])?;
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
            let dims_bin = encode_to_vec(&dims, standard())?;
            let row = self.client.query_opt("SELECT data FROM orthos WHERE dims = $1 LIMIT 1", &[&dims_bin])?;
            Ok(row.and_then(|r| {
                let data: Vec<u8> = r.get(0);
                decode_from_slice::<Ortho, _>(&data, standard()).ok().map(|(o, _)| o)
            }))
        } else {
            Ok(None)
        }
    }
    #[instrument(skip_all)]
    fn all_versions(&mut self) -> Result<Vec<usize>, FoldError> {
        let rows = self.client.query("SELECT DISTINCT version FROM orthos", &[])?;
        let mut versions: Vec<usize> = rows.into_iter().map(|r| {
            let version: i64 = r.get(0);
            version as usize
        }).collect();
        versions.sort_unstable();
        Ok(versions)
    }
    #[instrument(skip_all)]
    fn all_orthos(&mut self) -> Result<Vec<Ortho>, FoldError> {
        let rows = self.client.query("SELECT data FROM orthos", &[])?;
        Ok(rows.into_iter().filter_map(|r| {
            let data: Vec<u8> = r.get(0);
            decode_from_slice(&data, standard()).ok().map(|(o, _)| o)
        }).collect())
    }
    #[instrument(skip(self, ortho))]
    fn insert_or_update(&mut self, ortho: Ortho) -> Result<(), FoldError> {
        let id = ortho.id() as i64;
        let version = ortho.version() as i64;
        let dims = encode_to_vec(&ortho.dims(), standard())?;
        let data = encode_to_vec(&ortho, standard())?;
        self.client.execute(
            "INSERT INTO orthos (id, version, dims, data, claimed_at, claimed_by) VALUES ($1, $2, $3, $4, NULL, NULL)
             ON CONFLICT (id) DO UPDATE SET version = EXCLUDED.version, dims = EXCLUDED.dims, data = EXCLUDED.data, claimed_at = NULL, claimed_by = NULL",
            &[&id, &version, &dims, &data],
        )?;
        Ok(())
    }
    #[instrument(skip_all)]
    fn remove_by_id(&mut self, id: &usize) -> Result<(), FoldError> {
        let id = *id as i64;
        self.client.execute("DELETE FROM orthos WHERE id = $1", &[&id])?;
        Ok(())
    }
    #[instrument(skip_all)]
    fn len(&mut self) -> Result<usize, FoldError> {
        let row = self.client.query_one("SELECT COUNT(*) FROM orthos", &[])?;
        let count: i64 = row.get(0);
        Ok(count as usize)
    }
    #[instrument(skip_all)]
    fn sample_version(&mut self, _version: usize) -> Result<Option<Ortho>, FoldError> {
        let version = _version as i64;
        let follower_id = &self.follower_id;
        // Deterministic claim: pick lowest unclaimed id for version using index (version,id)
        let q = r#"
            UPDATE orthos o
            SET claimed_at = now(), claimed_by = $2
            WHERE o.id = (
              SELECT id FROM orthos
              WHERE version = $1 AND claimed_at IS NULL
              ORDER BY id
              FOR UPDATE SKIP LOCKED
              LIMIT 1
            )
            RETURNING o.data;
        "#;
        let row_opt = self.client.query_opt(q, &[&version, &follower_id])?;
        if let Some(row) = row_opt {
            let data: Vec<u8> = row.get(0);
            if let Ok((o, _)) = decode_from_slice::<Ortho, _>(&data, standard()) { return Ok(Some(o)); }
        }
        Ok(None)
    }
    #[instrument(skip_all)]
    fn version_counts(&mut self) -> Result<Vec<(usize, usize)>, FoldError> {
        let rows = self.client.query("SELECT version, COUNT(*) FROM orthos GROUP BY version ORDER BY version", &[])?;
        Ok(rows.into_iter().map(|r| {
            let v: i64 = r.get(0); let c: i64 = r.get(1); (v as usize, c as usize)
        }).collect())
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
