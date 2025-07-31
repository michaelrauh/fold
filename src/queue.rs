use crate::ortho::Ortho;
use amiquip::{Connection, QueueDeclareOptions, ConsumerMessage, ConsumerOptions, Exchange, Publish, FieldTable, AmqpValue};
use bincode::{encode_to_vec, decode_from_slice, config::standard};
use crossbeam_channel::TryRecvError;
use tracing::instrument;

// todo put in acks
// todo do not reopen connection for every push/pop
// todo revisit blocking implementation
pub trait QueueLike {
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
    pub url: String,
}

impl Queue {
    pub fn new(name: &str) -> Self {
        let url = std::env::var("FOLD_AMQP_URL").expect("FOLD_AMQP_URL environment variable must be set for Queue");
        Self {
            name: name.to_string(),
            url,
        }
    }

    #[instrument(skip_all)]
    pub fn len(&self) -> usize {
        let rabbit_url = std::env::var("FOLD_AMQP_URL")
            .expect("FOLD_AMQP_URL environment variable must be set for Queue");

        let mut connection = Connection::insecure_open(&rabbit_url).unwrap();
        let channel = connection.open_channel(None).unwrap();

        let queue = channel.queue_declare(
            self.name.as_str(),
            QueueDeclareOptions {
                durable: false, // todo make durable
                ..QueueDeclareOptions::default()
            },
        ).unwrap();
        let depth = queue.declared_message_count().unwrap();
        depth as usize
    }

    #[instrument(skip_all)]
    pub fn push_many(&mut self, orthos: Vec<Ortho>) {
        let mut conn = Connection::insecure_open(&self.url).unwrap();
        let channel = conn.open_channel(None).unwrap();
        let exchange = Exchange::direct(&channel);
        for ortho in orthos {
            let payload = encode_to_vec(&ortho, standard()).unwrap();
            exchange.publish(Publish::new(&payload, &self.name)).unwrap();
        }
        conn.close().unwrap();
    }

    #[instrument(skip_all)]
    pub fn pop_one(&mut self) -> Option<Ortho> {
        let mut conn = Connection::insecure_open(&self.url).unwrap();
        let channel = conn.open_channel(None).unwrap();
        let queue = channel.queue_declare(&self.name, QueueDeclareOptions::default()).unwrap();
        let consumer = queue.consume(ConsumerOptions::default()).unwrap();
        match consumer.receiver().try_recv() {
            Ok(msg) => {
                if let ConsumerMessage::Delivery(delivery) = msg {
                    let (ortho, _): (Ortho, _) = decode_from_slice(&delivery.body, standard()).ok()?;
                    consumer.ack(delivery).unwrap();
                    conn.close().unwrap();
                    
                    Some(ortho)
                } else {
                    conn.close().unwrap();
                    
                    None
                }
            },
            Err(TryRecvError::Empty) => {
                conn.close().unwrap();
                
                None
            },
            Err(TryRecvError::Disconnected) => {
                conn.close().unwrap();
                
                None
            }
        }
    }

    #[instrument(skip_all)]
    pub fn pop_many(&mut self, max: usize) -> Vec<Ortho> {
        
        let mut conn = Connection::insecure_open(&self.url).unwrap();
        let channel = conn.open_channel(None).unwrap();
        let queue = channel.queue_declare(&self.name, QueueDeclareOptions::default()).unwrap();
        let consumer = queue.consume(ConsumerOptions::default()).unwrap();
        let mut items = Vec::with_capacity(max);
        for _ in 0..max {
            match consumer.receiver().try_recv() {
                Ok(msg) => {
                    if let ConsumerMessage::Delivery(delivery) = msg {
                        if let Ok((ortho, _)) = decode_from_slice(&delivery.body, standard()) {
                            items.push(ortho);
                            consumer.ack(delivery).unwrap();
                        }
                    }
                },
                Err(TryRecvError::Empty) => {
                    
                    break;
                },
                Err(TryRecvError::Disconnected) => {
                    
                    break;
                }
            }
        }
        conn.close().unwrap();
        
        items
    }

    #[instrument(skip_all)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
        self.items.len()
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
}
