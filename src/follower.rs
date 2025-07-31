use crate::ortho_database::OrthoDatabaseLike;
use crate::queue::QueueLike;
use crate::InternerHolderLike;
use tracing::instrument;

pub struct Follower {
    low_version: Option<usize>,
    high_version: Option<usize>,
    low_interner: Option<crate::interner::Interner>,
    high_interner: Option<crate::interner::Interner>,
}

impl Follower {
    pub fn new() -> Self {
        Follower {
            low_version: None,
            high_version: None,
            low_interner: None,
            high_interner: None,
        }
    }

    #[instrument(skip_all)]
    pub fn run<Q: QueueLike, D: OrthoDatabaseLike, H: InternerHolderLike>(
        &mut self,
        db: &mut D,
        workq: &mut Q,
        holder: &mut H,
    ) {
        let versions = holder.versions();
        if versions.len() < 2 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            return;
        }

        let low_version = versions[0];
        let high_version = *versions.last().unwrap();

        if self.low_version != Some(low_version) {
            self.low_interner = holder.get(low_version);
            self.low_version = Some(low_version);
        }

        if self.high_version != Some(high_version) {
            self.high_interner = holder.get(high_version);
            self.high_version = Some(high_version);
        }

        let candidate = db.sample_version(low_version);
        if candidate.is_none() {
            holder.delete(low_version);
            self.low_interner = None;
            self.low_version = None;
            return;
        }
        let ortho = candidate.unwrap();
        let (_forbidden, prefixes) = ortho.get_requirements();
        let all_same = prefixes.iter().all(|prefix| {
                    self.low_interner.as_ref().and_then(|interner| interner.completions_for_prefix(prefix))
                        == self.high_interner.as_ref().and_then(|interner| interner.completions_for_prefix(prefix))
                });
        if all_same {
            let new_ortho = ortho.set_version(high_version);
            db.insert_or_update(new_ortho);
        } else {
            let new_ortho = ortho.set_version(high_version);
            workq.push_many(vec![new_ortho.clone()]);
            db.remove_by_id(&ortho.id());
        }
    }

}
// TODO panic on prefix not found in interner intersect - actually don't - prefixes may be missing. Just return replay.
// TODO follower should not delete interners if they can be found in the dbq. This leaves open the possibilitiy that version is in a worker, but that is a small enough amount that defaulting those to replay is OK
// TODO test