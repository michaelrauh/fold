pub struct Follower;

impl Follower {
    pub fn new() -> Self {
        Follower
    }

    pub(crate) fn remediate(
        &self,
        _work: &mut Vec<crate::ortho::Ortho>,
        _repository: &mut crate::repository::Repository,
        _interner: &mut crate::interner::Interner,
    ) {
        todo!()
    }
}
