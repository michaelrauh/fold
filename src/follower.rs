use crate::interner::InternerContainer;
use crate::ortho_database::OrthoDatabase;
use crate::queue::Queue;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::time;

pub struct Follower;

impl Follower {
    pub async fn run(
        db: Arc<OrthoDatabase>,
        workq: Arc<Queue>,
        container: Arc<Mutex<InternerContainer>>,
        shutdown: Arc<Notify>,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.notified() => {
                    break;
                }
                _ = async {
                    let guard = container.lock().await;
                    eprintln!("[Follower] main loop: interner_versions in container before removal: {:?}", guard.interners.keys().copied().collect::<Vec<_>>());
                    drop(guard);
                    if let Some(&lowest_version) = db.all_versions().await.first() {
                        Self::process_lowest_version(&db, &workq, &container, lowest_version).await;
                    }
                    Self::remove_unused_interners(&db, &container).await;
                    let guard = container.lock().await;
                    eprintln!("[Follower] main loop: interner_versions in container after removal: {:?}", guard.interners.keys().copied().collect::<Vec<_>>());
                    drop(guard);
                    time::sleep(std::time::Duration::from_millis(10)).await;
                } => {}
            }
        }
    }

    async fn process_lowest_version(
        db: &Arc<OrthoDatabase>,
        workq: &Arc<Queue>,
        container: &Arc<Mutex<InternerContainer>>,
        lowest_version: usize,
    ) {
        if let Some(ortho) = Self::get_ortho_for_version(db, lowest_version).await {
            let latest_version = container.lock().await.latest_version();
            if ortho.version() == latest_version {
                return;
            }
            let prefixes = ortho.prefixes();
            let all_same =
                Self::all_prefixes_same(&container, &prefixes, ortho.version(), latest_version)
                    .await;
            if all_same {
                Self::bump_ortho_version(db, ortho.clone(), latest_version).await;
            } else {
                Self::remove_ortho_and_enqueue(db, workq, ortho.clone(), latest_version).await;
            }
        }
    }

    async fn get_ortho_for_version(
        db: &Arc<OrthoDatabase>,
        version: usize,
    ) -> Option<crate::ortho::Ortho> {
        let orthos = db.all_orthos().await;
        orthos.into_iter().find(|o| o.version() == version)
    }

    async fn bump_ortho_version(
        db: &Arc<OrthoDatabase>,
        ortho: crate::ortho::Ortho,
        latest_version: usize,
    ) {
        let new_ortho = ortho.set_version(latest_version);
        db.insert_or_update(new_ortho).await;
    }

    async fn remove_ortho_and_enqueue(
        db: &Arc<OrthoDatabase>,
        workq: &Arc<Queue>,
        ortho: crate::ortho::Ortho,
        latest_version: usize,
    ) {
        let new_ortho = ortho.set_version(latest_version);
        db.remove_by_id(&new_ortho.id()).await;
        workq.push_many(vec![new_ortho]).await;
    }

    async fn remove_unused_interners(
        db: &Arc<OrthoDatabase>,
        container: &Arc<Mutex<InternerContainer>>,
    ) {
        let ortho_versions: HashSet<usize> = db.all_versions().await.into_iter().collect();
        let mut container_guard = container.lock().await;
        let interner_versions: HashSet<usize> =
            container_guard.interners.keys().copied().collect();
        let latest_version = container_guard.latest_version();
        eprintln!("[Follower] remove_unused_interners: ortho_versions in db: {:?}", ortho_versions);
        eprintln!("[Follower] remove_unused_interners: interner_versions in container: {:?}", interner_versions);
        eprintln!("[Follower] remove_unused_interners: latest_version in container: {}", latest_version);
        let to_remove = interner_versions
            .difference(&ortho_versions)
            .cloned()
            .filter(|v| *v != latest_version)
            .collect::<Vec<_>>();
        for version in &to_remove {
            eprintln!("[Follower] removing unused interner version: {}", version);
        }
        for version in to_remove {
            let _ = container_guard.remove_by_version(version);
        }
        eprintln!("[Follower] after removal, interner_versions in container: {:?}", container_guard.interners.keys().copied().collect::<Vec<_>>());
    }

    async fn all_prefixes_same(
        container: &Arc<Mutex<InternerContainer>>,
        prefixes: &[Vec<usize>],
        ortho_version: usize,
        latest_version: usize,
    ) -> bool {
        let container_guard = container.lock().await;
        prefixes.iter().all(|prefix| {
            container_guard.compare_prefix_bitsets(prefix.clone(), ortho_version, latest_version)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interner::InternerContainer;
    use crate::ortho::Ortho;
    // Removed unused imports: Queue, Notify, Runtime

    #[tokio::test]
    async fn test_remove_unused_interners_removes_versions_not_in_db() {
        let db = Arc::new(OrthoDatabase::new());
        let mut container = InternerContainer::from_text("a b");
        // Add a fake version not in db
        let fake_version = 99;
        let interner = container.get(container.latest_version()).add_text("c");
        container.interners.insert(fake_version, interner);
        let container = Arc::new(Mutex::new(container));
        // No orthos in db, so only the latest interner should remain
        Follower::remove_unused_interners(&db, &container).await;
        let guard = container.lock().await;
        let latest_version = guard.latest_version();
        // Only the latest version should remain
        assert_eq!(guard.interners.len(), 1);
        assert!(guard.interners.contains_key(&latest_version));
        assert!(!guard.interners.contains_key(&fake_version) || fake_version == latest_version);
    }

    #[tokio::test]
    async fn test_all_prefixes_same_true_and_false() {
        let mut container = InternerContainer::from_text("a b");
        let v1 = container.latest_version();
        let interner2 = container.get(v1).add_text("");
        let v2 = interner2.version();
        container.interners.insert(v2, interner2.clone());
        let container = Arc::new(Mutex::new(container));
        let prefixes = vec![vec![0]];
        // Should be true for identical bitsets
        let same = Follower::all_prefixes_same(&container, &prefixes, v1, v2).await;
        assert!(same);
        // Should be false for different bitsets
        let prefixes = vec![vec![1]]; // likely not present
        let not_same = Follower::all_prefixes_same(&container, &prefixes, v1, v2).await;
        assert!(!not_same);
    }

    #[tokio::test]
    async fn test_get_ortho_for_version_returns_correct_ortho() {
        let db = Arc::new(OrthoDatabase::new());
        let ortho = Ortho::new(42);
        db.insert_or_update(ortho.clone()).await;
        let found = Follower::get_ortho_for_version(&db, 42).await;
        assert_eq!(found, Some(ortho));
    }

    #[tokio::test]
    async fn test_bump_ortho_version_inserts_updated_ortho() {
        let db = Arc::new(OrthoDatabase::new());
        let ortho = Ortho::new(1);
        db.insert_or_update(ortho.clone()).await;
        Follower::bump_ortho_version(&db, ortho.clone(), 2).await;
        let found = db.get(&ortho.set_version(2).id()).await;
        assert_eq!(found.unwrap().version(), 2);
    }
}
