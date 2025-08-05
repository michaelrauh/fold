use crate::ortho::Ortho;
use amiquip::{Connection, QueueDeclareOptions, ConsumerMessage, ConsumerOptions, Exchange, Publish, Channel, Delivery};
use bincode::{encode_to_vec, decode_from_slice, config::standard};
use crossbeam_channel::TryRecvError;
use tracing::instrument;
use std::collections::VecDeque;

// Wrapper for deliveries that need to be acknowledged
#[allow(dead_code)] // delivery field is used for future manual acking
pub struct AckableOrtho {
    pub ortho: Ortho,
    delivery: Option<Delivery>,
}

impl AckableOrtho {
    pub fn new(ortho: Ortho, delivery: Option<Delivery>) -> Self {
        Self { ortho, delivery }
    }
    
    pub fn into_ortho(self) -> Ortho {
        self.ortho
    }
}

pub trait QueueLike: std::any::Any {
    fn push_many(&mut self, items: Vec<Ortho>);
    fn pop_one(&mut self) -> Option<Ortho>;
    fn pop_many(&mut self, max: usize) -> Vec<Ortho>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct Queue {
    pub name: String,
    connection: Option<Connection>,
    channel: Channel,
    pending_acks: VecDeque<Delivery>,
}

impl Queue {
    pub fn new(name: &str) -> Self {
        let url = std::env::var("FOLD_AMQP_URL").expect("FOLD_AMQP_URL environment variable must be set for Queue");
        let mut connection = Connection::insecure_open(&url).expect("Failed to open RabbitMQ connection");
        let channel = connection.open_channel(None).expect("Failed to open RabbitMQ channel");
        
        // Declare the queue as durable to persist messages
        let _queue = channel.queue_declare(
            name,
            QueueDeclareOptions {
                durable: true, // Made durable for persistence
                ..QueueDeclareOptions::default()
            },
        ).expect("Failed to declare queue");
        
        Self {
            name: name.to_string(),
            connection: Some(connection),
            channel,
            pending_acks: VecDeque::new(),
        }
    }

    // Ack all pending deliveries - should be called after processing
    pub fn ack_pending(&mut self) {
        while let Some(delivery) = self.pending_acks.pop_front() {
            if let Err(e) = delivery.ack(&self.channel) {
                eprintln!("Failed to ack message: {}", e);
            }
        }
    }

    // Pop one item with manual acking capability
    pub fn pop_one_ackable(&mut self) -> Option<AckableOrtho> {
        let queue = self.channel.queue_declare(&self.name, QueueDeclareOptions {
            durable: true,
            ..QueueDeclareOptions::default()
        }).ok()?;
        
        let consumer = queue.consume(ConsumerOptions::default()).ok()?;
        match consumer.receiver().try_recv() {
            Ok(msg) => {
                if let ConsumerMessage::Delivery(delivery) = msg {
                    let (ortho, _): (Ortho, _) = decode_from_slice(&delivery.body, standard()).ok()?;
                    Some(AckableOrtho::new(ortho, Some(delivery)))
                } else {
                    None
                }
            },
            Err(_) => None,
        }
    }

    // Pop many items with manual acking capability  
    pub fn pop_many_ackable(&mut self, max: usize) -> Vec<AckableOrtho> {
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
                            items.push(AckableOrtho::new(ortho, Some(delivery)));
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
    pub fn len(&self) -> usize {
        let queue = match self.channel.queue_declare(
            self.name.as_str(),
            QueueDeclareOptions {
                durable: true, // Made durable for persistence
                ..QueueDeclareOptions::default()
            },
        ) {
            Ok(q) => q,
            Err(_) => return 0,
        };
        
        queue.declared_message_count().unwrap_or(0) as usize
    }

    #[instrument(skip_all)]
    pub fn push_many(&mut self, orthos: Vec<Ortho>) {
        let exchange = Exchange::direct(&self.channel);
        for ortho in orthos {
            let payload = encode_to_vec(&ortho, standard()).unwrap();
            if let Err(e) = exchange.publish(Publish::new(&payload, &self.name)) {
                eprintln!("Failed to publish message: {}", e);
            }
        }
    }

    #[instrument(skip_all)]
    pub fn pop_one(&mut self) -> Option<Ortho> {
        let queue = self.channel.queue_declare(&self.name, QueueDeclareOptions {
            durable: true,
            ..QueueDeclareOptions::default()
        }).ok()?;
        
        let consumer = queue.consume(ConsumerOptions::default()).ok()?;
        match consumer.receiver().try_recv() {
            Ok(msg) => {
                if let ConsumerMessage::Delivery(delivery) = msg {
                    let (ortho, _): (Ortho, _) = decode_from_slice(&delivery.body, standard()).ok()?;
                    // Store delivery for manual acking later
                    self.pending_acks.push_back(delivery);
                    Some(ortho)
                } else {
                    None
                }
            },
            Err(_) => None,
        }
    }

    #[instrument(skip_all)]
    pub fn pop_many(&mut self, max: usize) -> Vec<Ortho> {
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
                            items.push(ortho);
                            // Store delivery for manual acking later
                            self.pending_acks.push_back(delivery);
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
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        // Ack any remaining pending messages before closing
        self.ack_pending();
        
        // Close the connection gracefully
        if let Some(connection) = self.connection.take() {
            if let Err(e) = connection.close() {
                eprintln!("Failed to close RabbitMQ connection: {}", e);
            }
        }
    }
}

impl QueueLike for Queue {
    #[instrument(skip_all)]
    fn push_many(&mut self, items: Vec<Ortho>) {
        self.push_many(items)
    }
    #[instrument(skip_all)]
    fn pop_one(&mut self) -> Option<Ortho> {
        self.pop_one()
    }
    #[instrument(skip_all)]
    fn pop_many(&mut self, max: usize) -> Vec<Ortho> {
        self.pop_many(max)
    }
    #[instrument(skip_all)]
    fn len(&self) -> usize {
        self.len()
    }
    #[instrument(skip_all)]
    fn is_empty(&self) -> bool {
        self.is_empty()
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
    fn push_many(&mut self, items: Vec<Ortho>) {
        self.items.extend(items);
    }
    fn pop_one(&mut self) -> Option<Ortho> {
        if self.items.is_empty() {
            None
        } else {
            Some(self.items.remove(0))
        }
    }
    fn pop_many(&mut self, max: usize) -> Vec<Ortho> {
        let mut out = Vec::new();
        for _ in 0..max {
            if let Some(item) = self.pop_one() {
                out.push(item);
            } else {
                break;
            }
        }
        out
    }
    fn len(&self) -> usize {
        let l = self.items.len();
        l
    }
    fn is_empty(&self) -> bool {
        self.items.is_empty()
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
        dbq.push_many(orthos.clone());
        // Pop first
        let popped1 = dbq.pop_one();
        assert!(popped1.is_some());
        assert_eq!(popped1.unwrap(), orthos[0]);
        // Pop second
        let popped2 = dbq.pop_one();
        assert!(popped2.is_some());
        assert_eq!(popped2.unwrap(), orthos[1]);
        // Pop empty
        let popped3 = dbq.pop_one();
        assert!(popped3.is_none());
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
            let mut queue = Queue::new(test_queue_name);
            let test_ortho = Ortho::new(42);
            queue.push_many(vec![test_ortho.clone()]);
            
            // Verify the queue has the item
            assert_eq!(queue.len(), 1);
            
            // Pop the item and verify it's correct
            let popped = queue.pop_one();
            assert!(popped.is_some());
            assert_eq!(popped.unwrap(), test_ortho);
            
            // Ack pending messages
            queue.ack_pending();
        } // Queue should be dropped and connection closed here
        
        // Create a new queue with the same name and verify it's empty (since we acked)
        {
            let queue = Queue::new(test_queue_name);
            assert_eq!(queue.len(), 0);
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
            let mut queue = Queue::new(test_queue_name);
            let test_ortho = Ortho::new(123);
            queue.push_many(vec![test_ortho.clone()]);
            
            // Pop the item but don't ack (it should stay in queue)
            let _popped = queue.pop_one();
            // Not calling queue.ack_pending() intentionally
        } // Queue dropped without acking
        
        // Create a new queue and verify the message is still there
        {
            let queue = Queue::new(test_queue_name);
            assert_eq!(queue.len(), 1, "Message should still be in durable queue after connection drop without ack");
        }
        
        // Clean up: pop and ack the message
        {
            let mut queue = Queue::new(test_queue_name);
            let _popped = queue.pop_one();
            queue.ack_pending();
        }
    }
}
