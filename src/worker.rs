use tracing::instrument;

pub struct Worker {
    pub interner: crate::interner::Interner,
}

impl Worker {
    pub fn new<H: crate::interner::InternerHolderLike>(container: &mut H) -> Self {
        let interner = container.get_latest().expect("No interner found");
        Worker { interner }
    }

    // todo batch pull 
    #[instrument(skip(self, workq, dbq, container))]
    pub fn run<Q: crate::queue::QueueLike, H: crate::interner::InternerHolderLike>(
        &mut self,
        workq: &mut Q,
        dbq: &mut Q,
        container: &mut H,
    ) {
        if let Some(ortho) = workq.pop_one() {
            if ortho.version() > self.interner.version() {
                println!("[worker] Updating interner from version {} to {} (ortho version {})", self.interner.version(), container.latest_version(), ortho.version());
                self.interner = container.get_latest().expect("No interner found");
            }
            let (forbidden, required) = ortho.get_requirements();
            let completions = self.interner.intersect(&required, &forbidden);
            let version = self.interner.version();
            for completion in completions {
                let new_orthos = ortho.add(completion, version);
                dbq.push_many(new_orthos);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;
    use crate::queue::{MockQueue, QueueLike};
    use crate::interner::{InMemoryInternerHolder, InternerHolderLike};

    #[test]
    fn test_worker_new_gets_latest_interner() {
        let _queue = MockQueue::new();
        let mut holder = InMemoryInternerHolder::with_seed("a b c", &mut crate::queue::MockQueue::new());
        let worker = Worker::new(&mut holder);
        let latest = holder.get_latest().unwrap();
        assert_eq!(worker.interner.version(), latest.version());
        assert_eq!(worker.interner.vocabulary(), latest.vocabulary());
    }

    #[test]
    fn test_worker_creates_orthos() {
        let _queue = MockQueue::new();
        let mut holder = InMemoryInternerHolder::with_seed("a b c", &mut crate::queue::MockQueue::new());
        let mut worker = Worker::new(&mut holder);
        let mut workq = MockQueue::new();
        let mut dbq = MockQueue::new();
        let ortho = Ortho::new(worker.interner.version());
        workq.push_many(vec![ortho.clone()]);
        worker.run(&mut workq, &mut dbq, &mut holder);
        let mut found = false;
        for _ in 0..10 {
            if dbq.pop_one().is_some() {
                found = true;
                break;
            }
        }
        assert!(found, "Worker should have created new orthos");
    }
}
