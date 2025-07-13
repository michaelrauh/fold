use crate::{feeder, follower};
use crate::interner::Interner;
use crate::ortho::Ortho;
use crate::repository::Repository;
use crate::splitter::Splitter;
use crate::worker::Worker;
use std::fs;

pub struct Processor;

impl Processor {
    pub fn new() -> Self {
        Processor
    }

    pub fn process(&self, file_path: &str) {
        let contents = fs::read_to_string(file_path).unwrap();
        let splitter = Splitter::new();
        let vocabulary = splitter.vocabulary(&contents);
        let phrases = splitter.phrases(&contents);

        let mut interner = Interner::new();
        interner.add(vocabulary, phrases);

        let ortho = Ortho::new(interner.version());
        let work = vec![ortho];
        
        let dbq: Vec<Ortho> = Vec::new();
        let repository = Repository::new();
        let feeder = feeder::Feeder::new();
        let follower = follower::Follower::new();

        Worker::process(work, dbq, interner, repository, feeder, follower);
    }
}
