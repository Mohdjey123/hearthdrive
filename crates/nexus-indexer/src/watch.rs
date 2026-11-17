use crate::Indexer;
use nexus_common::events::Event;
use nexus_common::types::ScanState;
use nexus_common::{NexusError, Result, time::now_ms};
use nexus_common::types::StorageRoot;
use nexus_storage::{StorageManager, paths};
use notify::{RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const DEBOUNCE: Duration = Duration::from_millis(750);
/// Beyond this many changed paths in one window, a full rescan is cheaper.
const MAX_PATHS_PER_WINDOW: usize = 2000;
const AVAILABILITY_INTERVAL: Duration = Duration::from_secs(15);

pub(crate) enum WatchMsg {
    Path(String, PathBuf),
    Rescan(String),
}

impl Indexer {
    pub fn watch_root(self: &Arc<Self>, root: &StorageRoot) -> Result<()> {
        let tx = self.watch_tx.clone();
        let root_id = root.id.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(event) => {
                if event.need_rescan() {
                    let _ = tx.send(WatchMsg::Rescan(root_id.clone()));
                }
                for path in event.paths {
                    let _ = tx.send(WatchMsg::Path(root_id.clone(), path));
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "filesystem watcher error; scheduling rescan");
                let _ = tx.send(WatchMsg::Rescan(root_id.clone()));
            }
        })
        .map_err(NexusError::internal)?;
        watcher
            .watch(Path::new(&root.absolute_path), RecursiveMode::Recursive)
            .map_err(NexusError::internal)?;
        self.watchers.lock().unwrap().insert(root.id.clone(), watcher);
        Ok(())
    }

    pub fn unwatch_root(&self, root_id: &str) {
        self.watchers.lock().unwrap().remove(root_id);
    }

    pub(crate) async fn run_watch_loop(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<WatchMsg>) {
        while let Some(first) = rx.recv().await {
            let mut pending: HashMap<String, HashSet<PathBuf>> = HashMap::new();
            let mut rescans: HashSet<String> = HashSet::new();
            let mut add = |msg: WatchMsg| match msg {
                WatchMsg::Path(root, path) => {
                    pending.entry(root).or_default().insert(path);
                }
                WatchMsg::Rescan(root) => {
                    rescans.insert(root);
                }
            };
            add(first);
            let deadline = tokio::time::Instant::now() + DEBOUNCE;
            while let Ok(Some(msg)) = tokio::time::timeout_at(deadline, rx.recv()).await {
                add(msg);
            }

            for (root_id, paths_changed) in &pending {
                if paths_changed.len() > MAX_PATHS_PER_WINDOW {
                    rescans.insert(root_id.clone());
                }
            }
            for root_id in &rescans {
                if let Ok(root) = self.db.get_root(root_id).await {
                    self.spawn_scan(root);
                }
            }
            for (root_id, changed) in pending {
                if rescans.contains(&root_id) {
                    continue;
                }
                let Ok(root) = self.db.get_root(&root_id).await else { continue };
                let base = PathBuf::from(&root.absolute_path);
                let rels = collapse(
                    changed
                        .iter()
                        .filter(|p| !self.is_ignored(p))
                        .filter_map(|p| paths::relative_of(&base, p))
                        .filter(|r| !r.is_empty() && !paths::is_hidden_internal(r))
                        .collect(),
                );
                for rel in rels {
                    if let Err(e) = self.reindex_path(&root, &rel).await {
                        tracing::debug!(path = %rel, error = %e, "reindex failed");
                    }
                }
            }
        }
    }

    /// Notices drives being unplugged or reconnected.
    pub(crate) async fn run_availability_monitor(self: Arc<Self>) {
        let mut tick = tokio::time::interval(AVAILABILITY_INTERVAL);
        loop {
            tick.tick().await;
            let Ok(roots) = self.db.list_roots().await else { continue };
            for root in roots.into_iter().filter(|r| r.enabled) {
                let online = StorageManager::is_online(&root);
                let previous = self.online.lock().unwrap().insert(root.id.clone(), online);
                match (previous, online) {
                    (Some(false), true) => {
                        tracing::info!(root = %root.name, "storage root reconnected");
                        if let Err(e) = self.watch_root(&root) {
                            tracing::warn!(error = %e, "could not watch reconnected root");
                        }
                        self.events.emit(Event::StorageChanged { root_id: root.id.clone() });
                        self.spawn_scan(root);
                    }
                    (Some(true), false) => {
                        tracing::info!(root = %root.name, "storage root went offline");
                        self.unwatch_root(&root.id);
                        let _ = self
                            .db
                            .set_scan_state(&ScanState {
                                root_id: root.id.clone(),
                                status: "offline".into(),
                                files_seen: 0,
                                bytes_seen: 0,
                                started_at: Some(now_ms()),
                                completed_at: None,
                                error: Some("The storage location is not connected".into()),
                            })
                            .await;
                        self.events.emit(Event::StorageChanged { root_id: root.id.clone() });
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Drops paths whose ancestor is also in the set (the ancestor's subtree
/// reindex covers them).
fn collapse(mut rels: Vec<String>) -> Vec<String> {
    rels.sort_by_key(|r| r.len());
    rels.dedup();
    let mut kept: Vec<String> = Vec::new();
    for rel in rels {
        let covered = kept.iter().any(|k| {
            rel.len() > k.len()
                && rel.as_bytes()[k.len()] == b'/'
                && rel.get(..k.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(k))
        });
        if !covered {
            kept.push(rel);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::collapse;

    #[test]
    fn collapse_keeps_only_top_most_paths() {
        let out = collapse(vec!["a/b/c".into(), "a".into(), "ab".into(), "x/y".into(), "A/z".into()]);
        assert_eq!(out, vec!["a".to_string(), "ab".to_string(), "x/y".to_string()]);
    }
}
