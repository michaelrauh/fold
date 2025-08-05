use crate::ortho::Ortho;
use crate::error::FoldError;
use amiquip::{Connection, QueueDeclareOptions, ConsumerMessage, ConsumerOptions, Exchange, Publish, Channel, Delivery};
use bincode::{encode_to_vec, decode_from_slice, config::standard};
use crossbeam_channel::TryRecvError;
use tracing::instrument;

// Trait for acknowledgment handles
pub trait AckHandle {
    fn ack(self) -> Result<(), FoldError>;
    fn nack(self) -> Result<(), FoldError>;
    fn ortho(&self) -> &Ortho;
}

// Since Channel cannot be cloned, we need a different approach
// Handle for real RabbitMQ deliveries that holds the delivery but not the channel
pub struct QueueHandle {
    ortho: Ortho,
    delivery: Option<Delivery>, // Option to allow moving out for ack
}

impl QueueHandle {
    pub fn new(ortho: Ortho, delivery: Delivery) -> Self {
        Self { ortho, delivery: Some(delivery) }
    }
    
    // Ack method that takes the channel - to be called by queue
    pub fn ack_with_channel(mut self, channel: &Channel) -> Result<(), FoldError> {
        if let Some(delivery) = self.delivery.take() {
            delivery.ack(channel)?;
        }
        Ok(())
    }
    
    // Nack method that takes the channel - to be called by queue
    pub fn nack_with_channel(mut self, channel: &Channel, requeue: bool) -> Result<(), FoldError> {
        if let Some(delivery) = self.delivery.take() {
            delivery.nack(channel, requeue)?;
        }
        Ok(())
    }
}

impl AckHandle for QueueHandle {
    fn ack(self) -> Result<(), FoldError> {
        // This can't work without the channel, so we'll make this an error
        Err(FoldError::Other("QueueHandle requires channel for ack - use queue.ack_handle() instead".into()))
    }
    
    fn nack(self) -> Result<(), FoldError> {
        // This can't work without the channel, so we'll make this an error
        Err(FoldError::Other("QueueHandle requires channel for nack - use queue.nack_handle() instead".into()))
    }
    
    fn ortho(&self) -> &Ortho {
        &self.ortho
    }
}

// No-op handle for mock queues
pub struct MockHandle {
    ortho: Ortho,
}

impl MockHandle {
    pub fn new(ortho: Ortho) -> Self {
        Self { ortho }
    }
}

impl AckHandle for MockHandle {
    fn ack(self) -> Result<(), FoldError> {
        // No-op for mock queues
        Ok(())
    }
    
    fn nack(self) -> Result<(), FoldError> {
        // No-op for mock queues
        Ok(())
    }
    
    fn ortho(&self) -> &Ortho {
        &self.ortho
    }
}

pub trait QueueLike: std::any::Any {
    type Handle: AckHandle;
    
    fn push_many(&mut self, items: Vec<Ortho>) -> Result<(), FoldError>;
    fn pop_one(&mut self) -> Option<Self::Handle>;
    fn pop_many(&mut self, max: usize) -> Vec<Self::Handle>;
    fn len(&self) -> Result<usize, FoldError>;
    fn is_empty(&self) -> Result<bool, FoldError> {
        Ok(self.len()? == 0)
    }
    // Add ack method to the trait
    fn ack_handle(&self, handle: Self::Handle) -> Result<(), FoldError>;
    fn nack_handle(&self, handle: Self::Handle, requeue: bool) -> Result<(), FoldError>;
}

pub struct Queue {
    pub name: String,
    connection: Option<Connection>,
    channel: Channel,
}

impl Queue {
    pub fn new(name: &str) -> Result<Self, FoldError> {
        let url = std::env::var("FOLD_AMQP_URL")
            .map_err(|_| FoldError::Other("FOLD_AMQP_URL environment variable must be set for Queue".into()))?;
        let mut connection = Connection::insecure_open(&url)
            .map_err(|e| FoldError::Queue(format!("Failed to open RabbitMQ connection: {}", e)))?;
        let channel = connection.open_channel(None)
            .map_err(|e| FoldError::Queue(format!("Failed to open RabbitMQ channel: {}", e)))?;
        
        // Declare the queue as durable to persist messages (this is idempotent)
        let _queue = channel.queue_declare(
            name,
            QueueDeclareOptions {
                durable: true,
                ..QueueDeclareOptions::default()
            },
        ).map_err(|e| FoldError::Queue(format!("Failed to declare queue: {}", e)))?;
        
        Ok(Self {
            name: name.to_string(),
            connection: Some(connection),
            channel,
        })
    }

    pub fn ack_handle(&self, handle: QueueHandle) -> Result<(), FoldError> {
        handle.ack_with_channel(&self.channel)
    }

    pub fn nack_handle(&self, handle: QueueHandle, requeue: bool) -> Result<(), FoldError> {
        handle.nack_with_channel(&self.channel, requeue)
    }

    #[instrument(skip_all)]
    pub fn pop_one(&mut self) -> Option<QueueHandle> {
        // Use existing declared queue - queue_declare is idempotent
        let queue = self.channel.queue_declare(&self.name, QueueDeclareOptions {
            durable: true,
            ..QueueDeclareOptions::default()
        }).ok()?;
        
        let consumer = queue.consume(ConsumerOptions::default()).ok()?;
        match consumer.receiver().try_recv() {
            Ok(msg) => {
                if let ConsumerMessage::Delivery(delivery) = msg {
                    let (ortho, _): (Ortho, _) = decode_from_slice(&delivery.body, standard()).ok()?;
                    Some(QueueHandle::new(ortho, delivery))
                } else {
                    None
                }
            },
            Err(_) => None,
        }
    }

    #[instrument(skip_all)]
    pub fn pop_many(&mut self, max: usize) -> Vec<QueueHandle> {
        let queue = match self.channel.queue_declare(&self.name, QueueDeclareOptions {
            durable: true,
            ..QueueDeclareOptions::default()
        }) {
            Ok(q) => q,
            Err(_) => return Vec::new(),
        };
        
        let consumer = match queue.consume(ConsumerOptions::default()) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        
        let mut items = Vec::with_capacity(max);
        for _ in 0..max {
            match consumer.receiver().try_recv() {
                Ok(msg) => {
                    if let ConsumerMessage::Delivery(delivery) = msg {
                        if let Ok((ortho, _)) = decode_from_slice(&delivery.body, standard()) {
                            items.push(QueueHandle::new(ortho, delivery));
                        }
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        items
    }

    #[instrument(skip_all)]
    pub fn len(&self) -> Result<usize, FoldError> {
        let queue = self.channel.queue_declare(
            &self.name,
            QueueDeclareOptions {
                durable: true,
                ..QueueDeclareOptions::default()
            },
        )?;
        
        queue.declared_message_count()
            .map(|count| count as usize)
            .ok_or_else(|| FoldError::Queue("Failed to get queue message count".to_string()))
    }

    #[instrument(skip_all)]
    pub fn push_many(&mut self, orthos: Vec<Ortho>) -> Result<(), FoldError> {
        let exchange = Exchange::direct(&self.channel);
        for ortho in orthos {
            let payload = encode_to_vec(&ortho, standard())?;
            exchange.publish(Publish::new(&payload, &self.name))?;
        }
        Ok(())
    }

    #[instrument(skip_all)]
    pub fn is_empty(&self) -> Result<bool, FoldError> {
        Ok(self.len()? == 0)
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        // Close the connection gracefully
        if let Some(connection) = self.connection.take() {
            if let Err(e) = connection.close() {
                eprintln!("Failed to close RabbitMQ connection: {}", e);
            }
        }
    }
}

impl QueueLike for Queue {
    type Handle = QueueHandle;
    
    #[instrument(skip_all)]
    fn push_many(&mut self, items: Vec<Ortho>) -> Result<(), FoldError> {
        self.push_many(items)
    }
    #[instrument(skip_all)]
    fn pop_one(&mut self) -> Option<Self::Handle> {
        self.pop_one()
    }
    #[instrument(skip_all)]
    fn pop_many(&mut self, max: usize) -> Vec<Self::Handle> {
        self.pop_many(max)
    }
    #[instrument(skip_all)]
    fn len(&self) -> Result<usize, FoldError> {
        self.len()
    }
    #[instrument(skip_all)]
    fn is_empty(&self) -> Result<bool, FoldError> {
        self.is_empty()
    }
    #[instrument(skip_all)]
    fn ack_handle(&self, handle: Self::Handle) -> Result<(), FoldError> {
        self.ack_handle(handle)
    }
    fn nack_handle(&self, handle: Self::Handle, requeue: bool) -> Result<(), FoldError> {
        self.nack_handle(handle, requeue)
    }
}

pub struct MockQueue {
    pub items: Vec<Ortho>,
}

impl MockQueue {
    pub fn new() -> Self {
        MockQueue { items: Vec::new() }
    }
}

impl QueueLike for MockQueue {
    type Handle = MockHandle;
    
    fn push_many(&mut self, items: Vec<Ortho>) -> Result<(), FoldError> {
        self.items.extend(items);
        Ok(())
    }
    fn pop_one(&mut self) -> Option<Self::Handle> {
        if self.items.is_empty() {
            None
        } else {
            let ortho = self.items.remove(0);
            Some(MockHandle::new(ortho))
        }
    }
    fn pop_many(&mut self, max: usize) -> Vec<Self::Handle> {
        let mut out = Vec::new();
        for _ in 0..max {
            if let Some(handle) = self.pop_one() {
                out.push(handle);
            } else {
                break;
            }
        }
        out
    }
    fn len(&self) -> Result<usize, FoldError> {
        Ok(self.items.len())
    }
    fn is_empty(&self) -> Result<bool, FoldError> {
        Ok(self.items.is_empty())
    }
    fn ack_handle(&self, handle: Self::Handle) -> Result<(), FoldError> {
        // No-op for mock queue - handle.ack() would do the same
        handle.ack()
    }
    fn nack_handle(&self, handle: Self::Handle, _requeue: bool) -> Result<(), FoldError> {
        // No-op for mock queue - handle.nack() would do the same
        handle.nack()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;

    #[test]
    fn test_push_many_and_pop_one() {
        let mut dbq = MockQueue::new();
        let orthos = vec![Ortho::new(1), Ortho::new(2)];
        dbq.push_many(orthos.clone()).expect("push_many should succeed");
        // Pop first
        let handle1 = dbq.pop_one();
        assert!(handle1.is_some());
        assert_eq!(*handle1.unwrap().ortho(), orthos[0]);
        // Pop second
        let handle2 = dbq.pop_one();
        assert!(handle2.is_some());
        assert_eq!(*handle2.unwrap().ortho(), orthos[1]);
        // Pop empty
        let handle3 = dbq.pop_one();
        assert!(handle3.is_none());
    }

    #[test]
    #[ignore] // Only run this test if RabbitMQ is available
    fn test_first_write_hits_queue() {
        // This test verifies that the first write to a real Queue actually works
        // Skip if FOLD_AMQP_URL is not set
        if std::env::var("FOLD_AMQP_URL").is_err() {
            eprintln!("Skipping test_first_write_hits_queue: FOLD_AMQP_URL not set");
            return;
        }

        let test_queue_name = "test_first_write_queue";
        
        // Create a queue and push one item
        {
            let mut queue = Queue::new(test_queue_name).expect("Queue creation should succeed");
            let test_ortho = Ortho::new(42);
            queue.push_many(vec![test_ortho.clone()]).expect("push_many should succeed");
            
            // Verify the queue has the item
            assert_eq!(queue.len().expect("len should succeed"), 1);
            
            // Pop the item and verify it's correct
            let handle = queue.pop_one();
            assert!(handle.is_some());
            let handle = handle.unwrap();
            assert_eq!(*handle.ortho(), test_ortho);
            
            // Ack the message
            queue.ack_handle(handle).expect("Failed to ack message");
        } // Queue should be dropped and connection closed here
        
        // Create a new queue with the same name and verify it's empty (since we acked)
        {
            let queue = Queue::new(test_queue_name).expect("Queue creation should succeed");
            assert_eq!(queue.len().expect("len should succeed"), 0);
        }
    }

    #[test]
    #[ignore] // Only run this test if RabbitMQ is available
    fn test_durable_queue_persistence() {
        // This test verifies that queues are durable and messages persist
        if std::env::var("FOLD_AMQP_URL").is_err() {
            eprintln!("Skipping test_durable_queue_persistence: FOLD_AMQP_URL not set");
            return;
        }

        let test_queue_name = "test_durable_queue";
        
        // Create a queue and push items without acking
        {
            let mut queue = Queue::new(test_queue_name).expect("Queue creation should succeed");
            let test_ortho = Ortho::new(123);
            queue.push_many(vec![test_ortho.clone()]).expect("push_many should succeed");
            
            // Pop the item but don't ack (it should stay in queue)
            let _handle = queue.pop_one();
            // Not calling queue.ack_handle() intentionally
        } // Queue dropped without acking
        
        // Create a new queue and verify the message is still there
        {
            let queue = Queue::new(test_queue_name).expect("Queue creation should succeed");
            assert_eq!(queue.len().expect("len should succeed"), 1, "Message should still be in durable queue after connection drop without ack");
        }
        
        // Clean up: pop and ack the message
        {
            let mut queue = Queue::new(test_queue_name).expect("Queue creation should succeed");
            let handle = queue.pop_one();
            if let Some(handle) = handle {
                queue.ack_handle(handle).expect("Failed to ack message");
            }
        }
    }
}
