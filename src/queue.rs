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
        let orthos = vec![Ortho::new(), Ortho::new()];
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
