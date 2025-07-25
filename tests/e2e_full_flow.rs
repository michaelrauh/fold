// Integration test for e2e full flow
use fold::feeder::OrthoFeeder;
use fold::follower::Follower;
use fold::interner::InternerHolder;
use fold::ortho_database::OrthoDatabase;
use fold::queue::Queue;
use fold::worker::Worker;
use std::sync::Arc;

#[tokio::test]
async fn test_e2e_full_flow() {
    let dbq = Arc::new(Queue::new("dbq", 10));
    let db = Arc::new(OrthoDatabase::new());
    let workq = Arc::new(Queue::new("workq", 10));
    let mut holder = InternerHolder::new(workq.clone());
    let feeder_handle = {
        let dbq = dbq.clone();
        let db = db.clone();
        let workq = workq.clone();
        tokio::spawn(async move {
            OrthoFeeder::run(dbq, db, workq).await;
        })
    };
    let follower_handle = {
        let db = db.clone();
        let workq = workq.clone();
        let container = Arc::new(holder.container.clone());
        tokio::spawn(async move {
            Follower::run(db, workq, container).await;
        })
    };
    let worker_handle = {
        let workq = workq.clone();
        let dbq = dbq.clone();
        let interner = holder
            .container
            .interners
            .values()
            .next()
            .cloned()
            .unwrap_or_else(|| fold::interner::Interner::from_text(""));
        tokio::spawn(async move {
            Worker::run(workq, dbq, interner).await;
        })
    };
    // Add first batch of text
    holder.add_text_with_seed("a b. c d. a c. b d.").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    // Add second batch of text
    holder
        .add_text_with_seed("e f. g h. e g. f h. a e. b f. c g. d h.")
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    // Check DB for an example with shape [2,2,2]
    let ortho_opt = db.get_by_dims(&[2, 2, 2]).await;
    assert!(
        ortho_opt.is_some(),
        "Should have at least one ortho with dims [2,2,2]"
    );
    drop(feeder_handle);
    drop(follower_handle);
    drop(worker_handle);
}
