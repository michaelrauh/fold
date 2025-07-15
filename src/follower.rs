pub struct Follower;

impl Default for Follower {
    fn default() -> Self {
        Self::new()
    }
}

impl Follower {
    pub fn new() -> Self {
        Follower
    }

    pub(crate) fn remediate(
        &self,
        _work: &mut [crate::ortho::Ortho],
        _repository: &mut crate::repository::Repository,
        _interner_registry: &mut crate::interner::InternerRegistry,
    ) {
        todo!()
    }
}
