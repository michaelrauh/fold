
pub mod error;
pub mod interner;
pub mod ortho;
pub mod ortho_database;
pub mod queue;
pub mod spatial;
pub mod splitter;

pub use error::*;
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
    ) -> Result<(), FoldError> {
        let versions = holder.versions();
        if versions.len() < 2 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            return Ok(());
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

        let candidate = db.sample_version(low_version)?;
        let ortho = match candidate {
            Some(o) => o,
            None => {
                holder.delete(low_version);
                self.low_interner = None;
                self.low_version = None;
                return Ok(());
            }
        };
        
        let (_forbidden, prefixes) = ortho.get_requirements();
        let all_same = prefixes.iter().all(|prefix| {
                    self.low_interner.as_ref().and_then(|interner| interner.completions_for_prefix(prefix))
                        == self.high_interner.as_ref().and_then(|interner| interner.completions_for_prefix(prefix))
                });
        if all_same {
            let new_ortho = ortho.set_version(high_version);
            db.insert_or_update(new_ortho)?;
        } else {
            let new_ortho = ortho.set_version(high_version);
            workq.push_many(vec![new_ortho.clone()])?;
            db.remove_by_id(&ortho.id())?;
        }
        Ok(())
    }
}

pub struct OrthoFeeder;

impl OrthoFeeder {
    #[instrument(skip_all)]
    pub fn run_feeder_once<Q: crate::queue::QueueLike, D: crate::ortho_database::OrthoDatabaseLike>(
        dbq: &mut Q,
        db: &mut D,
        workq: &mut Q,
    ) -> Result<(), FoldError>
    where 
        Q::Handle: crate::queue::AckHandle,
    {
        const BATCH_SIZE: usize = 1000;
        let handles = dbq.pop_many(BATCH_SIZE);
        if handles.is_empty() {
            return Ok(());
        }
        
        // Extract Orthos from handles for processing
        let items: Vec<crate::ortho::Ortho> = handles.iter().map(|h| h.ortho().clone()).collect();
        
        // Process the items and handle errors
        match db.upsert(items).and_then(|new_orthos| workq.push_many(new_orthos)) {
            Ok(()) => {
                // Both operations succeeded - ack all handles
                for handle in handles {
                    dbq.ack_handle(handle).map_err(|e| {
                        eprintln!("Failed to ack message: {}", e);
                        e
                    })?;
                }
                Ok(())
            }
            Err(e) => {
                eprintln!("Operation failed: {}", e);
                // Operation failed - nack and requeue all handles
                for handle in handles {
                    if let Err(nack_err) = dbq.nack_handle(handle, true) {
                        eprintln!("Failed to nack message: {}", nack_err);
                    }
                }
                Err(e)
            }
        }
    }
}

#[instrument(skip_all)]
pub fn run_worker_once<Q: queue::QueueLike, H: interner::InternerHolderLike>(
    workq: &mut Q,
    dbq: &mut Q,
    container: &mut H,
) -> Result<(), FoldError>
where 
    Q::Handle: queue::AckHandle,
{
    // println!("[worker] run_worker_once: workq.len()={}, dbq.len()={}", workq.len(), dbq.len());
    let handle = match workq.pop_one() {
        Some(h) => h,
        None => return Ok(()), // No work to do
    };
    
    let ortho = handle.ortho().clone();
    // println!("[worker] Popped ortho from workq: id={}, version={}", ortho.id(), ortho.version());
    
    // Get interner and handle potential failure
    let mut interner = container.get_latest().ok_or_else(|| {
        FoldError::Interner("No interner found".to_string())
    })?;
    
    if ortho.version() > interner.version() {
        println!("[worker] Updating interner from version {} to {} (ortho version {})", interner.version(), container.latest_version(), ortho.version());
        interner = container.get_latest().ok_or_else(|| {
            FoldError::Interner("No interner found after update".to_string())
        })?;
    }
    
    let (forbidden, required) = ortho.get_requirements();
    let completions = interner.intersect(&required, &forbidden);
    let version = interner.version();
    
    let mut new_orthos = Vec::new();
    for completion in completions {
        let mut batch = ortho.add(completion, version);
        new_orthos.append(&mut batch);
    }
    
    // Try to push to database queue and handle error appropriately
    match dbq.push_many(new_orthos) {
        Ok(()) => {
            // Success - ack the handle
            workq.ack_handle(handle).map_err(|e| {
                eprintln!("Failed to ack message: {}", e);
                e
            })?;
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to push to database queue: {}", e);
            // Failed to push - nack and requeue
            if let Err(nack_err) = workq.nack_handle(handle, true) {
                eprintln!("Failed to nack message: {}", nack_err);
            }
            Err(e)
        }
    }
}
