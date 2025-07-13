use crate::{feeder, follower, interner::Interner, repository};

pub struct Worker;

impl Worker {
    pub fn process(work: Vec<crate::ortho::Ortho>, mut dbq: Vec<crate::ortho::Ortho>, mut interner: Interner, mut repository: repository::Repository, mut feeder: feeder::Feeder, mut follower: follower::Follower) {
        let ortho = work.first().unwrap();
        let work_version = ortho.version();
        let interner_version = interner.version();

        if work_version > interner_version {
            interner = interner.update();
        }

        let (required, forbidden) = ortho.get_required_and_forbidden();
        let required_bits = interner.get_required_bits(&required);
        let forbidden_bits = interner.get_forbidden_bits(&forbidden);

        let results = interner.intersect(required_bits, forbidden_bits);

        let new_orthos = results.iter().map(|&result| { 
            ortho.add(result)
        });

        new_orthos.for_each(|new_ortho| {
            dbq.push(new_ortho);
        });

        feeder.feed(&mut dbq, &mut repository);

        follower.remediate(work, repository, interner);
    }
}
