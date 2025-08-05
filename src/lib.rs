
pub mod interner;
pub mod ortho;
pub mod ortho_database;
pub mod queue;
pub mod spatial;
pub mod splitter;

pub use interner::*;
pub use ortho_database::*;
pub use queue::*;
use tracing::instrument;

pub struct Follower {
    low_version: Option<usize>,
    high_version: Option<usize>,
    low_interner: Option<crate::interner::Interner>,
    high_interner: Option<crate::interner::Interner>,
}

impl Follower {
    pub fn new() -> Self {
        Follower {
            low_version: None,
            high_version: None,
            low_interner: None,
            high_interner: None,
        }
    }

    #[instrument(skip_all)]
    pub fn run_follower_once<Q: queue::QueueLike, D: ortho_database::OrthoDatabaseLike, H: interner::InternerHolderLike>(
        &mut self,
        db: &mut D,
        workq: &mut Q,
        holder: &mut H,
    ) {
        let versions = holder.versions();
        if versions.len() < 2 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            return;
        }

        let low_version = versions[0];
        let high_version = *versions.last().unwrap();

        if self.low_version != Some(low_version) {
            self.low_interner = holder.get(low_version);
            self.low_version = Some(low_version);
        }

        if self.high_version != Some(high_version) {
            self.high_interner = holder.get(high_version);
            self.high_version = Some(high_version);
        }

        let candidate = db.sample_version(low_version);
        if candidate.is_none() {
            holder.delete(low_version);
            self.low_interner = None;
            self.low_version = None;
            return;
        }
        let ortho = candidate.unwrap();
        let (_forbidden, prefixes) = ortho.get_requirements();
        let all_same = prefixes.iter().all(|prefix| {
                    self.low_interner.as_ref().and_then(|interner| interner.completions_for_prefix(prefix))
                        == self.high_interner.as_ref().and_then(|interner| interner.completions_for_prefix(prefix))
                });
        if all_same {
            let new_ortho = ortho.set_version(high_version);
            db.insert_or_update(new_ortho);
        } else {
            let new_ortho = ortho.set_version(high_version);
            if let Err(e) = workq.push_many(vec![new_ortho.clone()]) {
                eprintln!("Failed to push ortho to work queue in follower: {}", e);
                return; // Exit early if we can't push to queue
            }
            db.remove_by_id(&ortho.id());
        }
    }
}

pub struct OrthoFeeder;

impl OrthoFeeder {
    #[instrument(skip_all)]
    pub fn run_feeder_once<Q: crate::queue::QueueLike, D: crate::ortho_database::OrthoDatabaseLike>(
        dbq: &mut Q,
        db: &mut D,
        workq: &mut Q,
    ) where 
        Q::Handle: crate::queue::AckHandle,
    {
        const BATCH_SIZE: usize = 1000;
        let handles = dbq.pop_many(BATCH_SIZE);
        if !handles.is_empty() {
            // Extract Orthos from handles for processing
            let items: Vec<crate::ortho::Ortho> = handles.iter().map(|h| h.ortho().clone()).collect();
            
            // Process the items and handle errors
            match db.upsert(items) {
                Ok(new_orthos) => {
                    match workq.push_many(new_orthos) {
                        Ok(()) => {
                            // Both operations succeeded - ack all handles
                            for handle in handles {
                                if let Err(e) = dbq.ack_handle(handle) {
                                    eprintln!("Failed to ack message: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to push to work queue: {}", e);
                            // Push failed - nack and requeue all handles
                            for handle in handles {
                                if let Err(nack_err) = dbq.nack_handle(handle, true) {
                                    eprintln!("Failed to nack message: {}", nack_err);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to upsert to database: {}", e);
                    // Database operation failed - nack and requeue all handles
                    for handle in handles {
                        if let Err(nack_err) = dbq.nack_handle(handle, true) {
                            eprintln!("Failed to nack message: {}", nack_err);
                        }
                    }
                }
            }
        }
    }
}

#[instrument(skip_all)]
pub fn run_worker_once<Q: queue::QueueLike, H: interner::InternerHolderLike>(
    workq: &mut Q,
    dbq: &mut Q,
    container: &mut H,
) where 
    Q::Handle: queue::AckHandle,
{
    // println!("[worker] run_worker_once: workq.len()={}, dbq.len()={}", workq.len(), dbq.len());
    if let Some(handle) = workq.pop_one() {
        let ortho = handle.ortho().clone();
        // println!("[worker] Popped ortho from workq: id={}, version={}", ortho.id(), ortho.version());
        
        // Get interner and handle potential failure
        let mut interner = match container.get_latest() {
            Some(interner) => interner,
            None => {
                eprintln!("No interner found - nacking message");
                if let Err(e) = workq.nack_handle(handle, true) {
                    eprintln!("Failed to nack message: {}", e);
                }
                return;
            }
        };
        
        if ortho.version() > interner.version() {
            println!("[worker] Updating interner from version {} to {} (ortho version {})", interner.version(), container.latest_version(), ortho.version());
            interner = match container.get_latest() {
                Some(interner) => interner,
                None => {
                    eprintln!("No interner found after update - nacking message");
                    if let Err(e) = workq.nack_handle(handle, true) {
                        eprintln!("Failed to nack message: {}", e);
                    }
                    return;
                }
            };
        }
        
        let (forbidden, required) = ortho.get_requirements();
        let completions = interner.intersect(&required, &forbidden);
        let version = interner.version();
        
        let mut new_orthos = Vec::new();
        for completion in completions {
            let mut batch = ortho.add(completion, version);
            new_orthos.append(&mut batch);
        }
        
        // Try to push to database queue
        match dbq.push_many(new_orthos) {
            Ok(()) => {
                // Success - ack the handle
                if let Err(e) = workq.ack_handle(handle) {
                    eprintln!("Failed to ack message: {}", e);
                }
            }
            Err(e) => {
                eprintln!("Failed to push to database queue: {}", e);
                // Failed to push - nack and requeue
                if let Err(nack_err) = workq.nack_handle(handle, true) {
                    eprintln!("Failed to nack message: {}", nack_err);
                }
            }
        }
    } else {
        // println!("[worker] No ortho popped from workq");
    }
    // println!("[worker] run_worker_once end: workq.len()={}, dbq.len()={}", workq.len(), dbq.len());
}
