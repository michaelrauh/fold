use crate::interner::{Interner, InternerHolder};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

pub struct Worker {
    pub interner: Interner,
    pub container: Arc<Mutex<InternerHolder>>,
}

impl Worker {
    pub async fn new(container: Arc<Mutex<InternerHolder>>) -> Self {
        let interner = {
            let guard = container.lock().await;
            guard.get_latest().clone()
        };
        Worker {
            interner,
            container,
        }
    }

    pub async fn run(
        &mut self,
        workq: Arc<crate::queue::Queue>,
        dbq: Arc<crate::queue::Queue>,
        shutdown: Arc<Notify>,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.notified() => {
                    break;
                }
                _ = async {
                    if let Some(ortho) = workq.pop_one().await {
                        if ortho.version() > self.interner.version() {
                            println!("[worker] Updating interner from version {} to {} (ortho version {})", self.interner.version(), {
                                let guard = self.container.lock().await;
                                guard.latest_version()
                            }, ortho.version());
                            self.interner = {
                                let guard = self.container.lock().await;
                                guard.get_latest().clone()
                            };
                        }
                        let (forbidden, required) = ortho.get_requirements();
                        let completions = self.interner.intersect(&required, &forbidden);
                        let version = self.interner.version();
                        for completion in completions {
                            let new_orthos = ortho.add(completion, version);
                            let _ = dbq.push_many(new_orthos).await;
                        }
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
    use crate::interner::InternerHolder;
    use crate::ortho::Ortho;
    use crate::queue::Queue;
    use std::sync::Arc;
    use tokio::sync::{Mutex, Notify};

    #[tokio::test]
    async fn test_worker_new_gets_latest_interner() {
        let container = Arc::new(Mutex::new(InternerHolder::from_text(
            "a b c",
            Arc::new(Queue::new("test", 8)),
        )));
        let worker = Worker::new(container.clone()).await;
        let guard = container.lock().await;
        let latest = guard.get_latest();
        assert_eq!(worker.interner.version(), latest.version());
        assert_eq!(worker.interner.vocabulary(), latest.vocabulary());
    }

    #[tokio::test]
    async fn test_worker_updates_interner_if_out_of_date() {
        let mut holder = InternerHolder::from_text("a b", Arc::new(Queue::new("test", 8)));
        let interner1 = holder.get_latest().clone();
        let interner2 = interner1.add_text("c");
        holder
            .interners
            .insert(interner2.version(), interner2.clone());
        let container = Arc::new(Mutex::new(holder));
        let worker = Arc::new(Mutex::new(Worker::new(container.clone()).await));
        // Simulate out-of-date interner
        {
            let mut w = worker.lock().await;
            w.interner = interner1;
        }
        let workq = Arc::new(Queue::new("workq", 8));
        let dbq = Arc::new(Queue::new("dbq", 8));
        let shutdown = Arc::new(Notify::new());
        // Push an Ortho with a higher version to the work queue
        let ortho = Ortho::new(interner2.version());
        workq.push_many(vec![ortho]).await;
        // Run the worker in a background task
        let worker2 = worker.clone();
        let workq2 = workq.clone();
        let dbq2 = dbq.clone();
        let shutdown2 = shutdown.clone();
        let worker_handle = tokio::spawn(async move {
            let mut w = worker2.lock().await;
            w.run(workq2, dbq2, shutdown2).await;
        });
        // Give the worker a moment to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        shutdown.notify_waiters();
        worker_handle.await.unwrap();
        // Assert that the worker's interner is now the latest
        let w = worker.lock().await;
        assert_eq!(w.interner.version(), interner2.version());
    }

    #[tokio::test]
    async fn test_worker_creates_orthos() {
        let container = Arc::new(Mutex::new(InternerHolder::from_text(
            "a b c",
            Arc::new(Queue::new("test", 8)),
        )));
        let worker = Arc::new(Mutex::new(Worker::new(container.clone()).await));
        let workq = Arc::new(Queue::new("workq", 8));
        let dbq = Arc::new(Queue::new("dbq", 8));
        let shutdown = Arc::new(Notify::new());

        // Create an Ortho and push it to the work queue
        let ortho = Ortho::new(worker.lock().await.interner.version());
        workq.push_many(vec![ortho.clone()]).await;

        // Run the worker in a background task
        let worker2 = worker.clone();
        let workq2 = workq.clone();
        let dbq2 = dbq.clone();
        let shutdown2 = shutdown.clone();
        let worker_handle = tokio::spawn(async move {
            let mut w = worker2.lock().await;
            w.run(workq2, dbq2, shutdown2).await;
        });

        // Give the worker a moment to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        shutdown.notify_waiters();
        worker_handle.await.unwrap();

        // Check that dbq has new orthos using pop_one
        let mut found = false;
        for _ in 0..10 {
            if dbq.pop_one().await.is_some() {
                found = true;
                break;
            }
        }
        assert!(found, "Worker should have created new orthos");
    }
}
