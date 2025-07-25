use crate::ortho_database::OrthoDatabase;
use crate::queue::Queue;
use std::sync::Arc;
use tokio::time;

pub struct OrthoFeeder;

impl OrthoFeeder {
    pub async fn run(dbq: Arc<Queue>, db: Arc<OrthoDatabase>, workq: Arc<Queue>) {
        loop {
            let mut batch = Vec::new();
            // Drain all available items from dbq
            while let Ok(item) = dbq.receiver.lock().await.try_recv() {
                batch.push(item);
            }
            if !batch.is_empty() {
                let new_orthos = db.upsert(batch).await;
                for ortho in new_orthos {
                    let _ = workq.sender.as_ref().unwrap().send(ortho).await;
                }
            }
            time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
