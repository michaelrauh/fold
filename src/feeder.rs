use std::sync::Arc;
use crate::ortho_dbq::{OrthoDbQueue};
use crate::ortho_database::OrthoDatabase;
use crate::work_queue::WorkQueue;
use tokio::time;

pub struct OrthoFeeder;

impl OrthoFeeder {
    pub async fn run(dbq: Arc<OrthoDbQueue>, db: Arc<OrthoDatabase>, workq: Arc<WorkQueue>) {
        loop {
            let mut batch = Vec::new();
            // Drain all available items from dbq
            while let Ok(item) = dbq.receiver.lock().await.try_recv() {
                batch.push(item);
            }
            if !batch.is_empty() {
                let new_orthos = db.upsert(batch).await;
                for ortho in new_orthos {
                    let _ = workq.sender.send(ortho).await;
                }
            }
            time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
