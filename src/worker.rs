use crate::interner::{Interner, InternerHolder};

pub struct Worker {
    pub interner: Interner,
}

impl Worker {
    pub fn new(container: &mut InternerHolder) -> Self {
        let interner = container.get_latest().clone();
        Worker { interner }
    }

    // todo batch pull 
    pub fn run<Q: crate::queue::QueueLike>(
        &mut self,
        workq: &mut Q,
        dbq: &mut Q,
        container: &mut InternerHolder,
    ) {

        if let Some(ortho) = workq.pop_one() {
            if ortho.version() > self.interner.version() {
                println!("[worker] Updating interner from version {} to {} (ortho version {})", self.interner.version(), container.latest_version(), ortho.version());
                self.interner = container.get_latest().clone();
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
    use crate::interner::InternerHolder;
    use crate::ortho::Ortho;
    use crate::queue::{MockQueue, QueueLike};

    #[test]
    fn test_worker_new_gets_latest_interner() {
        let _queue = MockQueue::new();
        let mut holder = InternerHolder::from_text("a b c");
        let worker = Worker::new(&mut holder);
        let latest = holder.get_latest();
        assert_eq!(worker.interner.version(), latest.version());
        assert_eq!(worker.interner.vocabulary(), latest.vocabulary());
    }

    #[test]
    fn test_worker_updates_interner_if_out_of_date() {
        let _queue = MockQueue::new();
        let mut holder = InternerHolder::from_text("a b");
        let interner1 = holder.get_latest().clone();
        let interner2 = interner1.add_text("c");
        holder.interners.insert(interner2.version(), interner2.clone());
        let mut worker = Worker::new(&mut holder);
        worker.interner = interner1;
        let mut workq = MockQueue::new();
        let mut dbq = MockQueue::new();
        let ortho = Ortho::new(interner2.version());
        workq.push_many(vec![ortho]);
        worker.run(&mut workq, &mut dbq, &mut holder);
        assert_eq!(worker.interner.version(), interner2.version());
    }

    #[test]
    fn test_worker_creates_orthos() {
        let _queue = MockQueue::new();
        let mut holder = InternerHolder::from_text("a b c");
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
