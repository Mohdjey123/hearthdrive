use crate::Indexer;
use crate::scan::path_of;
use nexus_common::time::{now_ms, system_time_ms};
use nexus_storage::StorageManager;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const BATCH: i64 = 64;
const IDLE_WAIT: Duration = Duration::from_secs(30);
/// Pause between files so hashing never saturates the disk.
const PER_FILE_PAUSE: Duration = Duration::from_millis(15);

fn hash_file(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)?;
    Ok(hasher.finalize().to_hex().to_string())
}

impl Indexer {
    /// Computes BLAKE3 content hashes for files on online roots, slowly.
    pub(crate) async fn run_hasher(self: Arc<Self>) {
        // Let the first scans claim the disk before hashing starts.
        tokio::time::sleep(Duration::from_secs(10)).await;
        loop {
            let mut worked = false;
            let roots = self.db.list_roots().await.unwrap_or_default();
            for root in roots.into_iter().filter(StorageManager::is_online) {
                if self.is_scanning(&root.id) {
                    continue;
                }
                let Ok(batch) = self.db.unhashed_files(&root.id, BATCH).await else { continue };
                let base = PathBuf::from(&root.absolute_path);
                for item in batch {
                    worked = true;
                    let path = path_of(&base, &item.relative_path);
                    let expected = item.modified_at;
                    let outcome = tokio::task::spawn_blocking(move || {
                        let current = std::fs::metadata(&path).ok()?.modified().ok().and_then(system_time_ms);
                        if current != expected {
                            return Some(None); // changed since indexing; the watcher will catch up
                        }
                        Some(Some(hash_file(&path).ok()))
                    })
                    .await
                    .ok()
                    .flatten();
                    let hash = match outcome {
                        Some(None) => {
                            // Changed since indexing: refresh the entry, which resets its hash state.
                            let _ = self.reindex_path(&root, &item.relative_path).await;
                            tokio::time::sleep(PER_FILE_PAUSE).await;
                            continue;
                        }
                        Some(Some(hash)) => hash,
                        None => None,
                    };
                    let _ = self.db.set_hash(&item.id, hash.as_deref(), expected, now_ms()).await;
                    tokio::time::sleep(PER_FILE_PAUSE).await;
                }
            }
            if !worked {
                tokio::time::sleep(IDLE_WAIT).await;
            }
        }
    }
}
