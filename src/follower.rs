use crate::interner::InternerHolder;
use crate::ortho_database::OrthoDatabase;
use std::collections::HashSet;
use crate::queue::QueueLike;

pub struct Follower;

impl Follower {
    pub fn run<Q: QueueLike>(
        db: &mut OrthoDatabase,
        workq: &mut Q,
        container: &mut InternerHolder,
    ) {
        if let Some(&lowest_version) = db.all_versions().first() {
            Self::process_lowest_version(db, workq, container, lowest_version);
        }
        Self::remove_unused_interners(db, container);
    }

    fn process_lowest_version<Q: QueueLike>(
        db: &mut OrthoDatabase,
        workq: &mut Q,
        container: &mut InternerHolder,
        lowest_version: usize,
    ) {
        if let Some(ortho) = Self::get_ortho_for_version(db, lowest_version) {
            let latest_version = container.latest_version();
            if ortho.version() == latest_version {
                return;
            }
            let (_forbidden, prefixes) = ortho.get_requirements();
            let all_same = Self::all_prefixes_same(container, &prefixes, ortho.version(), latest_version);
            if all_same {
                Self::bump_ortho_version(db, ortho.clone(), latest_version);
            } else {
                Self::remove_ortho_and_enqueue(db, workq, ortho.clone(), latest_version);
            }
        }
    }

    fn get_ortho_for_version(
        db: &mut OrthoDatabase,
        version: usize,
    ) -> Option<crate::ortho::Ortho> {
        let orthos = db.all_orthos();
        orthos.into_iter().find(|o| o.version() == version)
    }

    fn bump_ortho_version(
        db: &mut OrthoDatabase,
        ortho: crate::ortho::Ortho,
        latest_version: usize,
    ) {
        let new_ortho = ortho.set_version(latest_version);
        db.insert_or_update(new_ortho);
    }

    fn remove_ortho_and_enqueue<Q: QueueLike>(
        db: &mut OrthoDatabase,
        workq: &mut Q,
        ortho: crate::ortho::Ortho,
        latest_version: usize,
    ) {
        let new_ortho = ortho.set_version(latest_version);
        db.remove_by_id(&new_ortho.id());
        workq.push_many(vec![new_ortho]);
    }

    fn remove_unused_interners(
        db: &mut OrthoDatabase,
        container: &mut InternerHolder,
    ) {
        let ortho_versions: HashSet<usize> = db.all_versions().into_iter().collect();
        let interner_versions: HashSet<usize> = container.interners.keys().copied().collect();
        let latest_version = container.latest_version();
        let to_remove = interner_versions
            .difference(&ortho_versions)
            .cloned()
            .filter(|v| *v != latest_version)
            .collect::<Vec<_>>();
        for version in to_remove {
            println!("[follower] Deleting interner version {}", version);
            let _ = container.remove_by_version(version);
        }
    }

    fn all_prefixes_same(
        container: &InternerHolder,
        prefixes: &[Vec<usize>],
        ortho_version: usize,
        latest_version: usize,
    ) -> bool {
        prefixes.iter().all(|prefix| {
            container.compare_prefix_bitsets(prefix.clone(), ortho_version, latest_version)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interner::InternerHolder;
    use crate::ortho::Ortho;

    #[test]
    fn test_remove_unused_interners_removes_versions_not_in_db() {
        let mut db = OrthoDatabase::new();
        let mut holder = InternerHolder::from_text("a b");
        // Add a fake version not in db
        let fake_version = 99;
        let interner = holder.get(holder.latest_version()).add_text("c");
        holder.interners.insert(fake_version, interner);
        // No orthos in db, so only the latest interner should remain
        Follower::remove_unused_interners(&mut db, &mut holder);
        let latest_version = holder.latest_version();
        // Only the latest version should remain
        assert_eq!(holder.interners.len(), 1);
        assert!(holder.interners.contains_key(&latest_version));
        assert!(!holder.interners.contains_key(&fake_version) || fake_version == latest_version);
    }

    #[test]
    fn test_all_prefixes_same_true_and_false() {
        let mut holder = InternerHolder::from_text("a b");
        let v1 = holder.latest_version();
        let interner2 = holder.get(v1).add_text("");
        let v2 = interner2.version();
        holder.interners.insert(v2, interner2.clone());
        let prefixes = vec![vec![0]];
        // Should be true for identical bitsets
        let same = Follower::all_prefixes_same(&holder, &prefixes, v1, v2);
        assert!(same);
        // Should be false for different bitsets
        let prefixes = vec![vec![1]]; // likely not present
        let not_same = Follower::all_prefixes_same(&holder, &prefixes, v1, v2);
        assert!(!not_same);
    }

    #[test]
    fn test_get_ortho_for_version_returns_correct_ortho() {
        let mut db = OrthoDatabase::new();
        let ortho = Ortho::new(42);
        db.insert_or_update(ortho.clone());
        let found = Follower::get_ortho_for_version(&mut db, 42);
        assert_eq!(found, Some(ortho));
    }

    #[test]
    fn test_bump_ortho_version_inserts_updated_ortho() {
        let mut db = OrthoDatabase::new();
        let ortho = Ortho::new(1);
        db.insert_or_update(ortho.clone());
        Follower::bump_ortho_version(&mut db, ortho.clone(), 2);
        let found = db.get(&ortho.set_version(2).id());
        assert_eq!(found.unwrap().version(), 2);
    }
}
// TODO panic on prefix not found in interner intersect - actually don't - prefixes may be missing. Just return replay.
// TODO follower should not delete interners if they can be found in the dbq. This leaves open the possibilitiy that version is in a worker, but that is a small enough amount that defaulting those to replay is OK