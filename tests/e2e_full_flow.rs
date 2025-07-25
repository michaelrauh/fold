// Integration test for e2e full flow
use fold::feeder::OrthoFeeder;
use fold::follower::Follower;
use fold::interner::InternerHolder;
use fold::interner::InternerContainer;
use fold::ortho_database::OrthoDatabase;
use fold::queue::Queue;
use fold::worker::Worker;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn test_e2e_full_flow() {
    let dbq = Arc::new(Queue::new("dbq", 10));
    // Ensure all collaborators share the same Arc<OrthoDatabase>
    let db = Arc::new(OrthoDatabase::new());
    let workq = Arc::new(Queue::new("e2e", 8));
    let container = Arc::new(Mutex::new(InternerContainer::from_text("a b. c d. a c. b d.")));
    let mut holder = InternerHolder { container: container.clone(), workq: workq.clone() };
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let feeder_shutdown = shutdown.clone();
    let follower_shutdown = shutdown.clone();
    let worker_shutdown = shutdown.clone();

    let feeder_handle = {
        let dbq = dbq.clone();
        let db = db.clone();
        let workq = workq.clone();
        let shutdown = feeder_shutdown.clone();
        tokio::spawn(async move {
            OrthoFeeder::run(dbq, db, workq, shutdown).await;
        })
    };
    let follower_handle = {
        let db = db.clone();
        let workq = workq.clone();
        let container = container.clone();
        let shutdown = follower_shutdown.clone();
        tokio::spawn(async move {
            Follower::run(db, workq, container, shutdown).await;
        })
    };
    let mut worker = Worker::new(container.clone()).await;
    let worker_handle = {
        let workq = workq.clone();
        let dbq = dbq.clone();
        let shutdown = worker_shutdown.clone();
        tokio::spawn(async move {
            worker.run(workq, dbq, shutdown).await;
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    holder.add_text_with_seed("a c e. b d f. c d. e f.").await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    // Wait for follower to remove version 1
    let mut waited = 0;
    let max_wait = 2000; // ms

    while holder.has_version(1).await && waited < max_wait {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        waited += 50;
    }
    let ortho_opt = db.get_optimal().await;
    // After upsert, print the map and seen set contents
    eprintln!("[e2e_full_flow] db map keys: {:?}", db.map.lock().await.keys().collect::<Vec<_>>());
    shutdown.notify_waiters(); // signal all tasks to exit

    feeder_handle.await.expect("feeder task panicked");
    follower_handle.await.expect("follower task panicked");
    worker_handle.await.expect("worker task panicked");
    assert!(
        ortho_opt.is_some(),
        "Should have at least one optimal ortho"
    );

    assert!(
        !holder.has_version(1).await,
        "Version 1 should be removed from the interner container"
    );
}
