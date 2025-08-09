use crate::ortho::Ortho;
use crate::error::FoldError;
use amiquip::{QueueDeclareOptions, ConsumerMessage, ConsumerOptions, Exchange, Publish};
use bincode::{encode_to_vec, decode_from_slice, config::standard};

pub trait QueueLenLike {
    fn len(&mut self) -> Result<usize, FoldError>;
    fn is_empty(&mut self) -> Result<bool, FoldError> {
        Ok(self.len()? == 0)
    }
}

pub trait QueueProducerLike: QueueLenLike {
    fn push_many(&mut self, items: Vec<Ortho>) -> Result<(), FoldError>;
}

pub trait QueueConsumerLike: QueueLenLike {
    fn consume_one_at_a_time_forever<F>(&mut self, callback: F) -> Result<(), FoldError>
    where
        F: FnMut(&Ortho) -> Result<(), FoldError>;
    fn consume_batch_forever<F>(&mut self, batch_size: usize, callback: F) -> Result<(), FoldError>
    where
        F: FnMut(&[Ortho]) -> Result<(), FoldError>;
}

pub struct QueueProducer {
    pub name: String,
    pub connection: amiquip::Connection,
}

impl QueueProducer {
    pub fn new(name: &str) -> Result<Self, FoldError> {
        let url = std::env::var("FOLD_AMQP_URL").expect("FOLD_AMQP_URL env var must be set");
        let mut connection = amiquip::Connection::insecure_open(&url)?;
        // Ensure queue exists BEFORE we ever publish so early publishes are not dropped.
        {
            let channel = connection.open_channel(None)?;
            let _ = channel.queue_declare(name, QueueDeclareOptions { durable: true, ..QueueDeclareOptions::default() })?;
        }
        Ok(Self {
            name: name.to_string(),
            connection,
        })
    }
}

impl QueueLenLike for QueueProducer {
    fn len(&mut self) -> Result<usize, FoldError> {
        let channel = self.connection.open_channel(None)?;
        let queue = channel.queue_declare(&self.name, QueueDeclareOptions {
            durable: true,
            ..QueueDeclareOptions::default()
        })?;
        Ok(queue.declared_message_count().unwrap_or(0) as usize)
    }
}

impl QueueProducerLike for QueueProducer {
    fn push_many(&mut self, orthos: Vec<Ortho>) -> Result<(), FoldError> {
        let count = orthos.len();
        if count == 0 { return Ok(()); }
        let channel = self.connection.open_channel(None)?;
        let exchange = Exchange::direct(&channel);
        for ortho in orthos {
            let payload = encode_to_vec(&ortho, standard())?;
            exchange.publish(Publish::new(&payload, &self.name))?;
        }
        println!("[queue][producer] pushed {} item(s) to {}", count, self.name);
        Ok(())
    }
}

pub struct QueueConsumer {
    pub name: String,
}

impl QueueConsumer {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
    fn get_url() -> String {
        std::env::var("FOLD_AMQP_URL").expect("FOLD_AMQP_URL env var must be set")
    }
}

impl QueueLenLike for QueueConsumer {
    fn len(&mut self) -> Result<usize, FoldError> {
        let url = QueueConsumer::get_url();
        let mut connection = amiquip::Connection::insecure_open(&url)?;
        let channel = connection.open_channel(None)?;
        let queue = channel.queue_declare(&self.name, QueueDeclareOptions {
            durable: true,
            ..QueueDeclareOptions::default()
        })?;
        Ok(queue.declared_message_count().unwrap_or(0) as usize)
    }
}

fn handle_delivery_result(result: &Result<(), FoldError>, delivery: amiquip::Delivery, channel: &amiquip::Channel) -> Result<(), FoldError> {
    if result.is_ok() {
        delivery.ack(channel)?;
    } else {
        delivery.nack(channel, true)?;
    }
    Ok(())
}

impl QueueConsumerLike for QueueConsumer {
    fn consume_one_at_a_time_forever<F>(&mut self, mut callback: F) -> Result<(), FoldError>
    where
        F: FnMut(&Ortho) -> Result<(), FoldError>,
    {
        let url = QueueConsumer::get_url();
        let mut connection = amiquip::Connection::insecure_open(&url)?;
        let channel = connection.open_channel(None)?;
        // Set prefetch (qos) to 1 to ensure fair dispatch across multiple workers
        channel.qos(0, 1, false)?; // prefetch_count = 1
        let queue = channel.queue_declare(&self.name, QueueDeclareOptions {
            durable: true,
            ..QueueDeclareOptions::default()
        })?;
        println!("[queue][consumer] starting consume loop on {} (prefetch=1)", self.name);
        let consumer = queue.consume(ConsumerOptions::default())?;
        for msg in consumer.receiver().iter() {
            if let ConsumerMessage::Delivery(delivery) = msg {
                if let Ok((ortho, _)) = decode_from_slice::<Ortho, _>(&delivery.body, standard()) {
                    println!("[queue][consumer] received ortho id={} version={} on {}", ortho.id(), ortho.version(), self.name);
                    let result = callback(&ortho);
                    handle_delivery_result(&result, delivery, &channel)?;
                } else {
                    println!("[queue][consumer] failed to decode message on {}", self.name);
                }
            }
        }
        consumer.cancel().ok();
        Ok(())
    }
    fn consume_batch_forever<F>(&mut self, batch_size: usize, mut callback: F) -> Result<(), FoldError>
    where
        F: FnMut(&[Ortho]) -> Result<(), FoldError>,
    {
        let url = QueueConsumer::get_url();
        let mut connection = amiquip::Connection::insecure_open(&url)?;
        let channel = connection.open_channel(None)?;
        channel.qos(0, batch_size as u16, false)?; // prefetch = batch_size
        let queue = channel.queue_declare(&self.name, QueueDeclareOptions {
            durable: true,
            ..QueueDeclareOptions::default()
        })?;
        println!("[queue][consumer] starting batch consume loop on {} (batch_size={})", self.name, batch_size);
        let consumer = queue.consume(ConsumerOptions::default())?;
        let mut batch = Vec::with_capacity(batch_size);
        let mut deliveries = Vec::with_capacity(batch_size);
        use std::time::{Duration, Instant};
        use crossbeam_channel::RecvTimeoutError;
        let flush_interval = Duration::from_secs(1);
        let mut last_flush = Instant::now();
        loop {
            // Try to receive with timeout to allow periodic flush
            match consumer.receiver().recv_timeout(flush_interval) {
                Ok(ConsumerMessage::Delivery(delivery)) => {
                    if let Ok((ortho, _)) = decode_from_slice(&delivery.body, standard()) {
                        batch.push(ortho);
                        deliveries.push(delivery);
                        if batch.len() % 250 == 0 || batch.len() == 1 {
                            println!("[queue][consumer] accumulating batch: size={} on {}", batch.len(), self.name);
                        }
                        if batch.len() >= batch_size {
                            println!("[queue][consumer] flushing full batch size={} on {}", batch.len(), self.name);
                            let result = callback(&batch);
                            for d in deliveries.drain(..) { handle_delivery_result(&result, d, &channel)?; }
                            batch.clear();
                            last_flush = Instant::now();
                        } else if !batch.is_empty() && last_flush.elapsed() >= flush_interval {
                            println!("[queue][consumer] flushing timed partial batch size={} on {}", batch.len(), self.name);
                            let result = callback(&batch);
                            for d in deliveries.drain(..) { handle_delivery_result(&result, d, &channel)?; }
                            batch.clear();
                            last_flush = Instant::now();
                        }
                    } else {
                        // decoding failed; requeue
                        handle_delivery_result(&Err(FoldError::Queue("decode failed".into())), delivery, &channel)?;
                    }
                }
                Ok(_) => { // cancellation variants
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !batch.is_empty() {
                        println!("[queue][consumer] flushing timed partial batch size={} on {}", batch.len(), self.name);
                        let result = callback(&batch);
                        for d in deliveries.drain(..) { handle_delivery_result(&result, d, &channel)?; }
                        batch.clear();
                    }
                    last_flush = Instant::now();
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        if !batch.is_empty() {
            println!("[queue][consumer] final flush size={} on {}", batch.len(), self.name);
            let result = callback(&batch);
            for d in deliveries.drain(..) { handle_delivery_result(&result, d, &channel)?; }
        }
        consumer.cancel().ok();
        Ok(())
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

impl QueueLenLike for MockQueue {
    fn len(&mut self) -> Result<usize, FoldError> {
        Ok(self.items.len())
    }
}

impl QueueProducerLike for MockQueue {
    fn push_many(&mut self, items: Vec<Ortho>) -> Result<(), FoldError> {
        self.items.extend(items);
        Ok(())
    }
}

impl QueueConsumerLike for MockQueue {
    fn consume_one_at_a_time_forever<F>(&mut self, mut callback: F) -> Result<(), FoldError>
    where
        F: FnMut(&Ortho) -> Result<(), FoldError> {
        if self.items.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            return Ok(());
        }
        let ortho = self.items.remove(0); // FIFO semantics
        callback(&ortho)?;
        Ok(())
    }
    fn consume_batch_forever<F>(&mut self, batch_size: usize, mut callback: F) -> Result<(), FoldError>
    where
        F: FnMut(&[Ortho]) -> Result<(), FoldError> {
        if self.items.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            return Ok(());
        }
        let take = usize::min(batch_size, self.items.len());
        let batch: Vec<Ortho> = self.items.drain(0..take).collect();
        callback(&batch)?;
        Ok(())
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
        dbq.push_many(orthos.clone()).expect("queue connection failed");
        // Pop first
        let handle1 = dbq.consume_one_at_a_time_forever(|ortho| {
            assert_eq!(ortho, &orthos[0]);
            Ok(())
        });
        assert!(handle1.is_ok());
        // Pop second
        let handle2 = dbq.consume_one_at_a_time_forever(|ortho| {
            assert_eq!(ortho, &orthos[1]);
            Ok(())
        });
        assert!(handle2.is_ok());
    }
}
