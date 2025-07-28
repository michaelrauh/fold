pub struct OrthoFeeder;

impl OrthoFeeder {
    pub fn run<Q: crate::queue::QueueLike, D: crate::ortho_database::OrthoDatabaseLike>(
        dbq: &mut Q,
        db: &mut D,
        workq: &mut Q,
    ) {
        const BATCH_SIZE: usize = 1000;
        let items = dbq.pop_many(BATCH_SIZE);
        if !items.is_empty() {
            let new_orthos = db.upsert(items);
            workq.push_many(new_orthos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{MockQueue, QueueLike};
    use crate::ortho_database::{InMemoryOrthoDatabase, OrthoDatabaseLike};
    use crate::ortho::Ortho;

    #[test]
    fn test_feeder_run_with_real_collaborators() {
        let mut dbq = MockQueue::new();
        let mut db = InMemoryOrthoDatabase::new();
        let mut workq = MockQueue::new();
        let ortho = Ortho::new(1);
        dbq.push_many(vec![ortho.clone()]);
        OrthoFeeder::run(&mut dbq, &mut db, &mut workq);
        assert!(db.get(&ortho.id()).is_some());
        assert!(workq.len() > 0);
    }
}