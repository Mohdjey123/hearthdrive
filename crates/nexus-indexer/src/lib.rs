//! Keeps the metadata index in step with the filesystem.
//!
//! * **Scanner** – incremental full scans; metadata only, batched writes.
//! * **Watcher** – debounced filesystem notifications re-index changed paths.
//! * **Hasher** – BLAKE3 hashes computed slowly in the background, never
//!   during the initial scan of a large drive.
//! * **Availability monitor** – notices drives being removed or reconnected.

mod hasher;
mod scan;
mod watch;

use nexus_common::events::EventBus;
use nexus_common::types::StorageRoot;
use nexus_database::Database;
use notify::RecommendedWatcher;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub use scan::ScanSummary;

pub struct Indexer {
    db: Database,
    events: EventBus,
    /// Paths never indexed even inside a root (e.g. HearthDrive's own data dir).
    ignored: Vec<PathBuf>,
    active_scans: Mutex<HashSet<String>>,
    watchers: Mutex<HashMap<String, RecommendedWatcher>>,
    online: Mutex<HashMap<String, bool>>,
    watch_tx: mpsc::UnboundedSender<watch::WatchMsg>,
    watch_rx: Mutex<Option<mpsc::UnboundedReceiver<watch::WatchMsg>>>,
}

impl Indexer {
    pub fn new(db: Database, events: EventBus, ignored: Vec<PathBuf>) -> Arc<Self> {
        let (watch_tx, watch_rx) = mpsc::unbounded_channel();
        let ignored = ignored
            .into_iter()
            .map(|p| std::fs::canonicalize(&p).map(dunce_simplify).unwrap_or(p))
            .collect();
        Arc::new(Self {
            db,
            events,
            ignored,
            active_scans: Mutex::new(HashSet::new()),
            watchers: Mutex::new(HashMap::new()),
            online: Mutex::new(HashMap::new()),
            watch_tx,
            watch_rx: Mutex::new(Some(watch_rx)),
        })
    }

    /// Starts the watcher, hasher and availability background tasks.
    pub fn start(self: &Arc<Self>) {
        if let Some(rx) = self.watch_rx.lock().unwrap().take() {
            tokio::spawn(self.clone().run_watch_loop(rx));
        }
        tokio::spawn(self.clone().run_hasher());
        tokio::spawn(self.clone().run_availability_monitor());
    }

    /// Begins tracking a root: watch it and bring its index up to date.
    pub fn register_root(self: &Arc<Self>, root: &StorageRoot) {
        let online = nexus_storage::StorageManager::is_online(root);
        self.online.lock().unwrap().insert(root.id.clone(), online);
        if online
            && let Err(e) = self.watch_root(root) {
                tracing::warn!(root = %root.name, error = %e, "could not watch storage root");
            }
        self.spawn_scan(root.clone());
    }

    pub fn unregister_root(&self, root_id: &str) {
        self.unwatch_root(root_id);
        self.online.lock().unwrap().remove(root_id);
    }

    pub fn is_scanning(&self, root_id: &str) -> bool {
        self.active_scans.lock().unwrap().contains(root_id)
    }

    pub fn spawn_scan(self: &Arc<Self>, root: StorageRoot) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.scan_root(&root).await {
                tracing::debug!(root = %root.name, error = %e, "scan not completed");
            }
        });
    }

    fn is_ignored(&self, path: &std::path::Path) -> bool {
        self.ignored.iter().any(|i| path.starts_with(i))
    }
}

/// Strips the `\\?\` verbatim prefix Windows canonicalisation adds.
fn dunce_simplify(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}
