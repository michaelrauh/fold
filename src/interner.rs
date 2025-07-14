pub struct Interner {}

impl Interner {
    pub fn new() -> Self {
        Interner {}
    }

    pub fn add(&mut self, _vocabulary: Vec<String>, _phrases: Vec<Vec<u16>>) {
        todo!()
    }

    pub fn version(&self) -> u64 {
        todo!()
    }

    pub fn update(&self) -> Interner {
        todo!()
    }

    pub(crate) fn get_required_bits(&self, _required: &[Vec<u16>]) -> Vec<u64> {
        todo!()
    }

    pub(crate) fn get_forbidden_bits(&self, _forbidden: &[u16]) -> Vec<u64> {
        todo!()
    }

    pub fn intersect(&self, _required: Vec<u64>, _forbidden: Vec<u64>) -> Vec<u16> {
        todo!()
    }
}
