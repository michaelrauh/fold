use crate::ortho_database::OrthoDatabase;
use crate::queue::Queue;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time;

pub struct OrthoFeeder;

impl OrthoFeeder {
    pub async fn run(
        dbq: Arc<Queue>,
        db: Arc<OrthoDatabase>,
        workq: Arc<Queue>,
        shutdown: Arc<Notify>,
    ) {
        const BATCH_SIZE: usize = 1000;
        loop {
            tokio::select! {
                _ = shutdown.notified() => {
                    break;
                }
                _ = async {
                    let items = dbq.pop_many(BATCH_SIZE).await;
                    if !items.is_empty() {
                        let new_orthos = db.upsert(items).await;
                        let _ = workq.push_many(new_orthos).await;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                } => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;
    use std::sync::Arc;
    use tokio::time::timeout;

    #[tokio::test]
    async fn test_feeder_run_with_real_collaborators() {
        async fn make_dbq_with_ortho(ortho: &Ortho) -> Arc<Queue> {
            let dbq = Arc::new(Queue::new("dbq", 2));
            dbq.push_many(vec![ortho.clone()]).await;
            dbq.close().await;
            dbq
        }
        let db = Arc::new(OrthoDatabase::new());
        let workq = Arc::new(Queue::new("workq", 2));
        let ortho = Ortho::new(42);
        let dbq = make_dbq_with_ortho(&ortho).await;
        let shutdown = Arc::new(Notify::new());
        let feeder_handle = {
            let dbq = dbq.clone();
            let db = db.clone();
            let workq = workq.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                OrthoFeeder::run(dbq, db, workq, shutdown).await;
            })
        };
        // Poll workq.pop_one() in a loop with a max timeout
        let mut popped_ortho = None;
        let start = std::time::Instant::now();
        let timeout_ms = 1000;
        while start.elapsed().as_millis() < timeout_ms && popped_ortho.is_none() {
            if let Ok(Some(o)) = timeout(std::time::Duration::from_millis(50), workq.pop_one()).await {
                popped_ortho = Some(o);
                break;
            }
        }
        assert_eq!(popped_ortho, Some(ortho.clone()), "Should pop ortho from workq within timeout");
        let fetched = db.get(&ortho.id()).await;
        assert_eq!(fetched, Some(ortho.clone()));
        feeder_handle.abort();
        // Test that duplicate ortho is not re-added
        let dbq2 = make_dbq_with_ortho(&ortho).await;
        let shutdown2 = Arc::new(Notify::new());
        let feeder_handle2 = {
            let dbq = dbq2.clone();
            let db = db.clone();
            let workq = workq.clone();
            let shutdown = shutdown2.clone();
            tokio::spawn(async move {
                OrthoFeeder::run(dbq, db, workq, shutdown).await;
            })
        };
        let popped2 = timeout(std::time::Duration::from_millis(100), workq.pop_one()).await;
        assert!(
            popped2.is_err() || popped2.unwrap().is_none(),
            "Should not pop duplicate ortho from workq"
        );
        feeder_handle2.abort();
    }
}
