pub struct Splitter;

impl Splitter {
    pub fn new() -> Self {
        Splitter
    }

    pub fn vocabulary(&self, _text: &str) -> Vec<String> {
        todo!("Implement vocabulary extraction")
    }

    pub fn phrases(&self, _text: &str) -> Vec<Vec<u16>> {
        todo!("Implement phrases extraction")
    }
}
