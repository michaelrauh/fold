pub struct Interner {
}

impl Interner {
    pub fn new() -> Self {
        Interner {
        }
    }

    pub fn add(&mut self, _vocabulary: Vec<String>, _phrases: Vec<Vec<u16>>) -> u64 {
       todo!()
    }

    pub fn version(&self) -> u64 {
        todo!()
    }
    
    pub fn update(&self) -> Interner {
        todo!()
    }
    
    pub(crate) fn get_required_bits(&self, required: &[Vec<u16>]) -> Vec<u64> {
        todo!()
    }
    
    pub(crate) fn get_forbidden_bits(&self, forbidden: &[u16]) -> Vec<u64> {
        todo!()
    }
    
    pub fn intersect(&self, required: Vec<u64>, forbidden: Vec<u64>) -> Vec<u16> {
        todo!()
    }
}
