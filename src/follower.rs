use std::sync::Arc;
use crate::interner::InternerContainer;
use crate::ortho_database::OrthoDatabase;
use crate::work_queue::WorkQueue;
use tokio::time;

pub struct Follower;

impl Follower {
    pub async fn run(db: Arc<OrthoDatabase>, workq: Arc<WorkQueue>, container: Arc<InternerContainer>) {
        loop {
            let map = db.map.lock().await;
            let mut versions: Vec<usize> = map.values().map(|o| o.version()).collect();
            versions.sort();
            versions.dedup();
            drop(map);
            while let Some(&lowest_version) = versions.first() {
                let map = db.map.lock().await;
                let ortho_opt = map.values().find(|o| o.version() == lowest_version).cloned();
                drop(map);
                if let Some(mut ortho) = ortho_opt {
                    let latest_version = container.latest_version();
                    if ortho.version() >= latest_version {
                        versions.remove(0);
                        continue;
                    }
                    let prefixes = ortho.prefixes();
                    let mut all_same = true;
                    for &prefix in &prefixes {
                        if !container.compare_prefix_bitsets(prefix, ortho.version(), latest_version) {
                            all_same = false;
                            break;
                        }
                    }
                    if all_same {
                        ortho.set_version(latest_version);
                        let mut map = db.map.lock().await;
                        map.insert(ortho.id(), ortho);
                    } else {
                        ortho.set_version(latest_version);
                        let mut map = db.map.lock().await;
                        map.remove(&ortho.id());
                        let _ = workq.sender.send(ortho).await;
                    }
                }
                let map = db.map.lock().await;
                versions = map.values().map(|o| o.version()).collect();
                drop(map);
                versions.sort();
                versions.dedup();
            }
            time::sleep(std::time::Duration::from_millis(10)).await;
            // todo revisit the above code
            // todo make it delete obsolete versions from interner
        }
    }
}
