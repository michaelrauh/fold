use crate::interner::{Interner, InternerRegistry};
use crate::logical_coords::LogicalCoordinateCache;
use crate::ortho::Ortho;
use crate::repository::Repository;
use crate::splitter::Splitter;
use crate::worker::Worker;
use crate::{feeder, follower};
use std::{fs, vec};

pub struct Processor;

impl Default for Processor {
    fn default() -> Self {
        Self::new()
    }
}

impl Processor {
    pub fn new() -> Self {
        Processor
    }

    pub fn process(&self, file_path: &str) {
        let mut interner = configure_interner(file_path);
        let mut interner_registry = InternerRegistry::new();
        let seed = Ortho::new(interner.version());
        let mut work = vec![seed];
        let mut dbq: Vec<Ortho> = Vec::new();
        let mut repository = Repository::new();
        let feeder = feeder::Feeder::new();
        let follower = follower::Follower::new();
        let mut cache = LogicalCoordinateCache::new();

        loop {
            if work.is_empty() {
                break;
            }
            let cur = work.pop().unwrap();
            if cur.version() > interner.version() {
                interner = interner_registry.get_latest();
            }
    
            let new_orthos = Worker::process(cur, &interner, &mut cache);
    
            dbq.extend(new_orthos);
    
            feeder.feed(&mut dbq, &mut work, &mut repository);
    
            follower.remediate(&mut work, &mut repository, &mut interner_registry);
        }
    }
}

fn configure_interner(file_path: &str) -> Interner {
    let contents = fs::read_to_string(file_path).unwrap();
    let splitter = Splitter::new();
    let vocabulary = splitter.vocabulary(&contents);
    let phrases = splitter.phrases(&contents);

    let interner = Interner::new();
    interner.add(vocabulary, phrases)
}
