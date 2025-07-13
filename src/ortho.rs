#[derive(Debug)]
pub struct Ortho {
}

impl Ortho {
    pub fn new(_version: u64) -> Self {
        Ortho {
        }
    }

    pub fn version(&self) -> u64 {
        todo!()
    }
    
    pub(crate) fn get_required_and_forbidden(&self) -> (Vec<Vec<u16>>, Vec<u16>) {
        todo!()
    }
    
    pub(crate) fn add(&self, _to_add: u16) -> Ortho {
        todo!()
    }
}
