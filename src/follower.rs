

pub struct Follower;

impl Follower{
    pub fn new() -> Self {
        Follower
    }
    
    pub(crate) fn remediate(&self, _work: Vec<crate::ortho::Ortho>, _repository: crate::repository::Repository, _interner: crate::interner::Interner) {
        todo!()
    }
}