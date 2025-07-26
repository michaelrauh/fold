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
        loop {
            tokio::select! {
                _ = shutdown.notified() => {
                    break;
                }
                _ = async {
                    let mut batch = Vec::new();
                    // Drain dbq as much as possible, but yield if no item is available quickly
                    loop {
                        match tokio::time::timeout(std::time::Duration::from_millis(1), dbq.pop_one()).await {
                            Ok(Some(item)) => batch.push(item),
                            Ok(None) | Err(_) => break,
                        }
                    }
                    if !batch.is_empty() {
                        let new_orthos = db.upsert(batch).await;
                        let _ = workq.push_many(new_orthos).await;
                    }
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
        let popped = timeout(std::time::Duration::from_millis(100), workq.pop_one()).await;
        assert!(popped.is_ok(), "Should pop from workq");
        let popped_ortho = popped.unwrap();
        assert_eq!(popped_ortho, Some(ortho.clone()));
        let fetched = db.get(&ortho.id()).await;
        assert_eq!(fetched, Some(ortho.clone()));
        feeder_handle.abort();
        // Now push the same ortho again, should not be put back in workq
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
