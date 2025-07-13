use std::fs;

pub struct Processor;

impl Processor {
    pub fn new() -> Self {
        Processor
    }
    
    pub fn process(&self, file_path: &str) {
        let contents = fs::read_to_string(file_path).unwrap();
        println!("{}", contents);
    }
}
