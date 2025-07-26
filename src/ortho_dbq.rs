use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use crate::ortho::Ortho;

pub struct Queue {
    pub name: String,
    pub sender: Option<mpsc::Sender<Ortho>>,
    pub receiver: Arc<Mutex<mpsc::Receiver<Ortho>>>,
}

impl Queue {
    pub fn new(name: &str, buffer: usize) -> Self {
        let (sender, receiver) = mpsc::channel(buffer);
        Self {
            name: name.to_string(),
            sender: Some(sender),
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    pub async fn push_many(&self, orthos: Vec<Ortho>) {
        let sender = self.sender.as_ref().expect("Queue is closed");
        for ortho in orthos {
            let _ = sender.send(ortho).await;
        }
    }

    pub async fn pop_one(&self) -> Option<Ortho> {
        let mut receiver = self.receiver.lock().await;
        receiver.recv().await
    }

    pub fn close(&mut self) {
        self.sender = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;
    use crate::ortho::Ortho;

    #[test]
    fn test_push_many_and_pop_one() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let mut dbq = Queue::new("test", 10);
            let orthos = vec![Ortho::new(1), Ortho::new(2)];
            dbq.push_many(orthos.clone()).await;

            // Pop first
            let popped1 = dbq.pop_one().await;
            assert!(popped1.is_some());
            assert_eq!(popped1.unwrap(), orthos[0]);

            dbq.close(); // Close the sender to drop it
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
