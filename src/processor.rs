use crate::interner::Interner;
use crate::ortho::Ortho;
use crate::repository::Repository;
use crate::splitter::Splitter;
use crate::worker::Worker;
use crate::{feeder, follower};
use std::{fs, vec};

pub struct Processor;

impl Processor {
    pub fn new() -> Self {
        Processor
    }

    pub fn process(&self, file_path: &str) {
        let mut interner = configure_interner(file_path);
        let seed = Ortho::new(interner.version());
        let mut work = vec![seed];
        let mut dbq: Vec<Ortho> = Vec::new();
        let mut repository = Repository::new();
        let feeder = feeder::Feeder::new();
        let follower = follower::Follower::new();

        loop {
            if work.is_empty() {
                break;
            }
            let cur = work.pop().unwrap();
            if &cur.version() > &interner.version() {
                interner = interner.update();
            }
    
            let new_orthos = Worker::process(cur, &mut interner);
    
            dbq.extend(new_orthos);
    
            feeder.feed(&mut dbq, &mut work, &mut repository);
    
            follower.remediate(&mut work, &mut repository, &mut interner);
        }
    }
}

fn configure_interner(file_path: &str) -> Interner {
    let contents = fs::read_to_string(file_path).unwrap();
    let splitter = Splitter::new();
    let vocabulary = splitter.vocabulary(&contents);
    let phrases = splitter.phrases(&contents);

    let mut interner = Interner::new();
    interner.add(vocabulary, phrases);
    interner
}
