// Integration test for e2e full flow
use fold::feeder::OrthoFeeder;
use fold::follower::Follower;
use fold::interner::InternerHolder;
use fold::ortho_database::OrthoDatabase;
use fold::queue::Queue;
use fold::worker::Worker;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn test_e2e_full_flow() {
    let dbq = Arc::new(Queue::new("dbq", 10));
    let db = Arc::new(OrthoDatabase::new());
    let workq = Arc::new(Queue::new("e2e", 8));
    let holder = Arc::new(Mutex::new(InternerHolder::from_text(
        "a b. c d. a c. b d.",
        workq.clone(),
    )));
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
        let container = holder.clone();
        let shutdown = follower_shutdown.clone();
        tokio::spawn(async move {
            Follower::run(db, workq, container, shutdown).await;
        })
    };
    let mut worker = Worker::new(holder.clone()).await;
    let worker_handle = {
        let workq = workq.clone();
        let dbq = dbq.clone();
        let shutdown = worker_shutdown.clone();
        tokio::spawn(async move {
            worker.run(workq, dbq, shutdown).await;
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    holder
        .lock()
        .await
        .add_text_with_seed("a c e. b d f. c d. e f.")
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let mut waited = 0;
    let max_wait = 2000; // ms

    while holder.lock().await.has_version(1).await && waited < max_wait {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        waited += 50;
    }
    let ortho_opt = db.get_optimal().await;

    shutdown.notify_waiters();

    feeder_handle.await.expect("feeder task panicked");
    follower_handle.await.expect("follower task panicked");
    worker_handle.await.expect("worker task panicked");
    assert!(
        ortho_opt.is_some(),
        "Should have at least one optimal ortho"
    );

    assert!(
        !holder.lock().await.has_version(1).await,
        "Version 1 should be removed from the interner container"
    );
    println!(
        "Test completed successfully with optimal ortho: {:?}",
        ortho_opt
    );
}
