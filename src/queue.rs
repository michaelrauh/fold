use crate::ortho::Ortho;
use crate::error::FoldError;

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
    /// Try to consume up to `batch_size` items once in a non-blocking fashion.
    /// Returns the number of items processed (0 if none available).
    fn try_consume_batch_once<F>(&mut self, batch_size: usize, callback: F) -> Result<usize, FoldError>
    where
        F: FnMut(&[Ortho]) -> Result<(), FoldError>;
}

// In-memory wrappers and mock queue. The project no longer depends on RabbitMQ/amiquip.

pub struct QueueProducer {
    pub name: String,
    pub inner: MockQueue,
}

impl QueueProducer {
    pub fn new(name: &str) -> Result<Self, FoldError> {
        Ok(Self { name: name.to_string(), inner: MockQueue::new() })
    }
}

impl QueueProducer {
    pub fn save_to_path(&mut self, path: &std::path::Path) -> Result<(), FoldError> {
        self.inner.save_to_path(path)
    }
    pub fn load_from_path(&mut self, path: &std::path::Path) -> Result<(), FoldError> {
        self.inner.load_from_path(path)
    }
}

impl QueueLenLike for QueueProducer {
    fn len(&mut self) -> Result<usize, FoldError> {
        self.inner.len()
    }
}

impl QueueProducerLike for QueueProducer {
    fn push_many(&mut self, items: Vec<Ortho>) -> Result<(), FoldError> {
        self.inner.push_many(items)
    }
}

pub struct QueueConsumer {
    pub name: String,
    pub inner: MockQueue,
}

impl QueueConsumer {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), inner: MockQueue::new() }
    }
}

impl QueueConsumer {
    pub fn save_to_path(&mut self, path: &std::path::Path) -> Result<(), FoldError> {
        self.inner.save_to_path(path)
    }
    pub fn load_from_path(&mut self, path: &std::path::Path) -> Result<(), FoldError> {
        self.inner.load_from_path(path)
    }
}

impl QueueLenLike for QueueConsumer {
    fn len(&mut self) -> Result<usize, FoldError> {
        self.inner.len()
    }
}

impl QueueConsumerLike for QueueConsumer {
    fn consume_one_at_a_time_forever<F>(&mut self, mut callback: F) -> Result<(), FoldError>
    where
        F: FnMut(&Ortho) -> Result<(), FoldError>,
    {
        loop {
            if self.inner.items.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            let ortho = self.inner.items.remove(0);
            callback(&ortho)?;
        }
    }

    fn consume_batch_forever<F>(&mut self, batch_size: usize, mut callback: F) -> Result<(), FoldError>
    where
        F: FnMut(&[Ortho]) -> Result<(), FoldError>,
    {
        loop {
            if self.inner.items.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            let take = usize::min(batch_size, self.inner.items.len());
            let batch: Vec<Ortho> = self.inner.items.drain(0..take).collect();
            callback(&batch)?;
        }
    }

    fn try_consume_batch_once<F>(&mut self, batch_size: usize, mut callback: F) -> Result<usize, FoldError>
    where
        F: FnMut(&[Ortho]) -> Result<(), FoldError>,
    {
        if self.inner.items.is_empty() {
            return Ok(0);
        }
        let take = usize::min(batch_size, self.inner.items.len());
        let batch: Vec<Ortho> = self.inner.items.drain(0..take).collect();
        callback(&batch)?;
        Ok(batch.len())
    }
}

pub struct MockQueue {
    pub items: Vec<Ortho>,
}

impl MockQueue {
    pub fn new() -> Self {
        MockQueue { items: Vec::new() }
    }

    pub fn save_to_path(&self, path: &std::path::Path) -> Result<(), FoldError> {
        let bytes = bincode::encode_to_vec(&self.items, bincode::config::standard())?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn load_from_path(&mut self, path: &std::path::Path) -> Result<(), FoldError> {
        let data = std::fs::read(path)?;
        let (items, _): (Vec<Ortho>, usize) = bincode::decode_from_slice(&data, bincode::config::standard())?;
        self.items = items;
        Ok(())
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

    fn try_consume_batch_once<F>(&mut self, batch_size: usize, mut callback: F) -> Result<usize, FoldError>
    where
        F: FnMut(&[Ortho]) -> Result<(), FoldError> {
        if self.items.is_empty() {
            return Ok(0);
        }
        let take = usize::min(batch_size, self.items.len());
        let batch: Vec<Ortho> = self.items.drain(0..take).collect();
        callback(&batch)?;
        Ok(batch.len())
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
