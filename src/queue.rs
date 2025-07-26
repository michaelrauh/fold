use crate::ortho::Ortho;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

pub struct Queue {
    pub name: String,
    sender: Arc<Mutex<Option<mpsc::Sender<Ortho>>>>,
    pub receiver: Arc<Mutex<mpsc::Receiver<Ortho>>>,
    front_buffer: Arc<Mutex<Vec<Ortho>>>,
}

impl Queue {
    pub fn new(name: &str, buffer: usize) -> Self {
        let (sender, receiver) = mpsc::channel(2305843009213693951);
        Self {
            name: name.to_string(),
            sender: Arc::new(Mutex::new(Some(sender))),
            receiver: Arc::new(Mutex::new(receiver)),
            front_buffer: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn push_many(&self, orthos: Vec<Ortho>) {
        let sender_guard = self.sender.lock().await;
        if let Some(sender) = sender_guard.as_ref() {
            for ortho in orthos {
                let _ = sender.try_send(ortho);
            }
        }
    }

    pub async fn push_front(&self, orthos: Vec<Ortho>) {
        let mut front = self.front_buffer.lock().await;
        for ortho in orthos.into_iter().rev() {
            front.insert(0, ortho);
        }
    }

    pub async fn pop_one(&self) -> Option<Ortho> {
        let mut front = self.front_buffer.lock().await;
        if !front.is_empty() {
            return Some(front.remove(0));
        }
        drop(front);
        let mut receiver = self.receiver.lock().await;
        receiver.try_recv().ok()
    }

    /// Tries to pop up to `max` items from the queue without blocking. Returns as many as are available (may be empty).
    pub async fn pop_many(&self, max: usize) -> Vec<Ortho> {
        let mut items = Vec::with_capacity(max);
        let mut front = self.front_buffer.lock().await;
        while !front.is_empty() && items.len() < max {
            items.push(front.remove(0));
        }
        drop(front);
        if items.len() < max {
            let mut receiver = self.receiver.lock().await;
            for _ in items.len()..max {
                match receiver.try_recv() {
                    Ok(item) => items.push(item),
                    Err(_) => break,
                }
            }
        }
        items
    }

    pub async fn close(&self) {
        let mut sender_guard = self.sender.lock().await;
        *sender_guard = None;
    }

    /// Returns true if the queue is empty.
    pub async fn is_empty(&self) -> bool {
        let receiver = self.receiver.lock().await;
        receiver.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;
    use tokio::runtime::Runtime;

    #[test]
    fn test_push_many_and_pop_one() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let dbq = Queue::new("test", 10);
            let orthos = vec![Ortho::new(1), Ortho::new(2)];
            dbq.push_many(orthos.clone()).await;

            // Pop first
            let popped1 = dbq.pop_one().await;
            assert!(popped1.is_some());
            assert_eq!(popped1.unwrap(), orthos[0]);

            dbq.close().await; // Close the sender to drop it
            // Pop second
            let popped2 = dbq.pop_one().await;
            assert!(popped2.is_some());
            assert_eq!(popped2.unwrap(), orthos[1]);

            // Pop empty
            let popped3 = dbq.pop_one().await;
            assert!(popped3.is_none());
        });
    }
}
