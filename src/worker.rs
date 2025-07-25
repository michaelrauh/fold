use crate::interner::{Interner, InternerContainer};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

pub struct Worker {
    pub interner: Interner,
    pub container: Arc<Mutex<InternerContainer>>,
}

impl Worker {
    pub async fn new(container: Arc<Mutex<InternerContainer>>) -> Self {
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
                    let ortho = workq.pop_one().await;
                    if let Some(ortho) = ortho {
                        if ortho.version() > self.interner.version() {
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
                            dbq.push_many(new_orthos).await;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                } => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interner::InternerContainer;
    use crate::queue::Queue;
    use crate::ortho::Ortho;
    use std::sync::Arc;
    use tokio::sync::{Mutex, Notify};

    #[tokio::test]
    async fn test_worker_new_gets_latest_interner() {
        let container = Arc::new(Mutex::new(InternerContainer::from_text("a b c")));
        let worker = Worker::new(container.clone()).await;
        let guard = container.lock().await;
        let latest = guard.get_latest();
        assert_eq!(worker.interner.version(), latest.version());
        assert_eq!(worker.interner.vocabulary(), latest.vocabulary());
    }

    #[tokio::test]
    async fn test_worker_updates_interner_if_out_of_date() {
        let mut container = InternerContainer::from_text("a b");
        let interner1 = container.get_latest().clone();
        let interner2 = interner1.add_text("c");
        container.interners.insert(interner2.version(), interner2.clone());
        let container = Arc::new(Mutex::new(container));
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
        let container = Arc::new(Mutex::new(InternerContainer::from_text("a b c")));
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
