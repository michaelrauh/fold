use crate::ortho::Ortho;

pub struct Feeder;

impl Feeder {
    pub fn new() -> Self {
        Feeder
    }

    pub(crate) fn feed(&self, _dbq: &mut Vec<Ortho>, _work: &mut Vec<Ortho>, _repository: &mut crate::repository::Repository) {
        todo!()
    }
}
