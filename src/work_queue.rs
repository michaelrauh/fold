use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use crate::ortho::Ortho;

pub struct WorkQueue {
    pub sender: mpsc::Sender<Ortho>,
    pub receiver: Arc<Mutex<mpsc::Receiver<Ortho>>>,
}

impl WorkQueue {
    pub fn new(buffer: usize) -> Self {
        let (sender, receiver) = mpsc::channel(buffer);
        Self {
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }
}
