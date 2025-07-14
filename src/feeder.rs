use crate::ortho::Ortho;

pub struct Feeder;

impl Default for Feeder {
    fn default() -> Self {
        Self::new()
    }
}

impl Feeder {
    pub fn new() -> Self {
        Feeder
    }

    pub(crate) fn feed(&self, _dbq: &mut [Ortho], _work: &mut [Ortho], _repository: &mut crate::repository::Repository) {
        todo!()
    }
}
