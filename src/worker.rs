// Stub Worker implementation for orchestrated tests
use crate::interner::Interner;
use crate::ortho::Ortho;
use std::sync::Arc;

pub struct Worker {
    pub interner: Interner,
}

impl Worker {
    pub fn new(interner: Interner) -> Self {
        Worker { interner }
    }
    pub async fn run(workq: Arc<crate::work_queue::WorkQueue>, dbq: Arc<crate::ortho_dbq::OrthoDbQueue>, interner: Interner) {
        loop {
            let ortho = {
                let mut receiver = workq.receiver.lock().await;
                receiver.try_recv().ok()
            };
            if let Some(ortho) = ortho {
                let (forbidden, required) = ortho.get_requirements();
                let completions = interner.intersect(&required, &forbidden);
                for completion in completions {
                    let new_orthos = ortho.add(completion, ortho.version());
                    for new_ortho in new_orthos {
                        dbq.push_many(vec![new_ortho]).await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    pub fn pull_from_work_queue(&mut self) -> Option<Ortho> {
        // Silence unused warning for now
        let _ = &self.interner;
        todo!("Stub: pull from work queue")
    }
    pub fn get_requirements(&self, _ortho: &Ortho) -> (Vec<usize>, Vec<Vec<usize>>) {
        todo!("Stub: get requirements from ortho")
    }
    pub fn solve_requirements(&self, _required: &Vec<Vec<usize>>, _forbidden: &Vec<usize>) -> Vec<usize> {
        todo!("Stub: call interner.intersect")
    }
    pub fn add_to_ortho_and_dbq(&self, _ortho: &Ortho, _completions: Vec<usize>) {
        todo!("Stub: call add on ortho and insert into dbq")
    }
}
