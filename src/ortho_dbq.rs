// Simple in-memory, single-threaded, synchronous components for ortho database queue pattern.
// OrthoDbQueue: queue for incoming items
// OrthoFeeder: reads from queue, upserts to db, writes new items to work queue
// OrthoWorkQueue: queue for new work
// OrthoDatabase: in-memory db with upsert

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use crate::ortho::Ortho;
use crate::work_queue::WorkQueue;
use crate::feeder::OrthoFeeder;
use crate::follower::Follower;

pub struct OrthoDbQueue {
    pub sender: mpsc::Sender<Ortho>,
    pub receiver: Arc<Mutex<mpsc::Receiver<Ortho>>>,
}

impl OrthoDbQueue {
    pub fn new(buffer: usize) -> Self {
        let (sender, receiver) = mpsc::channel(buffer);
        Self {
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ortho_database::OrthoDatabase;
    use super::*;
    use std::sync::Arc;
    // Removed test_e2e_full_flow, now in tests/e2e_full_flow.rs
}
