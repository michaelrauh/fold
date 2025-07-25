use crate::ortho::Ortho;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

pub struct Queue {
    pub name: String,
    sender: Arc<Mutex<Option<mpsc::Sender<Ortho>>>>,
    pub receiver: Arc<Mutex<mpsc::Receiver<Ortho>>>,
}

impl Queue {
    pub fn new(name: &str, buffer: usize) -> Self {
        let (sender, receiver) = mpsc::channel(buffer);
        Self {
            name: name.to_string(),
            sender: Arc::new(Mutex::new(Some(sender))),
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    pub async fn push_many(&self, orthos: Vec<Ortho>) {
        let sender_guard = self.sender.lock().await;
        if let Some(sender) = sender_guard.as_ref() {
            for ortho in orthos {
                let _ = sender.send(ortho).await;
            }
        }
    }

    pub async fn pop_one(&self) -> Option<Ortho> {
        let mut receiver = self.receiver.lock().await;
        receiver.recv().await
    }

    pub async fn close(&self) {
        let mut sender_guard = self.sender.lock().await;
        *sender_guard = None;
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
