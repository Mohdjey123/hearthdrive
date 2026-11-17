use crate::Indexer;
use nexus_common::events::Event;
use nexus_common::time::{now_ms, system_time_ms};
use nexus_common::types::{NewEntry, ScanState, StorageRoot};
use nexus_common::{NexusError, Result};
use nexus_storage::paths;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use walkdir::WalkDir;

const BATCH_SIZE: usize = 2000;

#[derive(Debug, Clone, Copy, Default)]
pub struct ScanSummary {
    pub files: u64,
    pub bytes: u64,
    pub removed: u64,
    pub unreadable: u64,
}

struct ActiveGuard<'a> {
    set: &'a Mutex<std::collections::HashSet<String>>,
    id: String,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.id);
    }
}

pub(crate) fn entry_from_meta(rel: String, meta: &Metadata) -> NewEntry {
    let (parent, name) = paths::split_parent(&rel);
    let is_dir = meta.is_dir();
    let (extension, mime_type) = paths::describe(&name, is_dir);
    NewEntry {
        relative_path: rel,
        parent_path: parent,
        filename: name,
        is_dir,
        extension,
        mime_type,
        size: if is_dir { 0 } else { meta.len() as i64 },
        created_at: meta.created().ok().and_then(system_time_ms),
        modified_at: meta.modified().ok().and_then(system_time_ms),
    }
}

pub(crate) fn path_of(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(part);
    }
    p
}

/// Walks `start` (inside `base`) on a blocking thread, streaming batches of
/// entries. Never follows symlinks or junctions. Returns unreadable count.
fn walk(base: &Path, start: &Path, ignored: &[PathBuf], tx: &mpsc::Sender<Vec<NewEntry>>) -> u64 {
    let mut batch = Vec::with_capacity(BATCH_SIZE);
    let mut unreadable = 0;
    let mut it = WalkDir::new(start).min_depth(1).follow_links(false).into_iter();
    while let Some(next) = it.next() {
        let entry = match next {
            Ok(e) => e,
            Err(err) => {
                unreadable += 1;
                tracing::trace!(error = %err, "skipping unreadable entry");
                continue;
            }
        };
        let path = entry.path();
        let file_type = entry.file_type();
        let Some(rel) = paths::relative_of(base, path) else { continue };
        if paths::is_hidden_internal(&rel) || ignored.iter().any(|i| path.starts_with(i)) {
            if file_type.is_dir() {
                it.skip_current_dir();
            }
            continue;
        }
        if file_type.is_symlink() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        batch.push(entry_from_meta(rel, &meta));
        if batch.len() >= BATCH_SIZE && tx.blocking_send(std::mem::take(&mut batch)).is_err() {
            return unreadable;
        }
    }
    if !batch.is_empty() {
        let _ = tx.blocking_send(batch);
    }
    unreadable
}

impl Indexer {
    /// Incremental full scan: upserts everything found, then drops entries
    /// that were not seen. Metadata only; hashing happens later.
    pub async fn scan_root(self: &Arc<Self>, root: &StorageRoot) -> Result<ScanSummary> {
        if !self.active_scans.lock().unwrap().insert(root.id.clone()) {
            return Err(NexusError::Conflict("A scan is already running for this root".into()));
        }
        let _guard = ActiveGuard { set: &self.active_scans, id: root.id.clone() };

        let started = now_ms();
        let clock = Instant::now();
        let base = PathBuf::from(&root.absolute_path);
        let mut state = ScanState {
            root_id: root.id.clone(),
            status: "scanning".into(),
            files_seen: 0,
            bytes_seen: 0,
            started_at: Some(started),
            completed_at: None,
            error: None,
        };

        if !root.enabled || !base.is_dir() {
            state.status = "offline".into();
            state.error = Some("The storage location is not connected".into());
            self.db.set_scan_state(&state).await?;
            self.events.emit(Event::StorageChanged { root_id: root.id.clone() });
            return Err(NexusError::Unavailable(format!("{} is offline", root.name)));
        }

        self.db.set_scan_state(&state).await?;
        self.events.emit(Event::ScanStarted { root_id: root.id.clone() });

        let (tx, mut rx) = mpsc::channel::<Vec<NewEntry>>(4);
        let ignored = self.ignored.clone();
        let walk_base = base.clone();
        let walker = tokio::task::spawn_blocking(move || walk(&walk_base, &walk_base, &ignored, &tx));

        let mut summary = ScanSummary::default();
        let mut last_progress = Instant::now();
        let mut failure: Option<NexusError> = None;
        while let Some(batch) = rx.recv().await {
            summary.files += batch.len() as u64;
            summary.bytes += batch.iter().map(|e| e.size.max(0) as u64).sum::<u64>();
            if let Err(e) = self.db.upsert_entries(&root.id, &batch, started).await {
                failure = Some(e);
                break;
            }
            if last_progress.elapsed() > Duration::from_millis(500) {
                last_progress = Instant::now();
                state.files_seen = summary.files as i64;
                state.bytes_seen = summary.bytes as i64;
                let _ = self.db.set_scan_state(&state).await;
                self.events.emit(Event::ScanProgress {
                    root_id: root.id.clone(),
                    files_seen: summary.files,
                    bytes_seen: summary.bytes,
                });
            }
        }
        drop(rx);
        summary.unreadable = walker.await.map_err(NexusError::internal)?;

        // A drive unplugged mid-scan looks like "everything vanished": keep the index.
        if failure.is_none() && !base.is_dir() {
            failure = Some(NexusError::Unavailable(format!("{} went offline during the scan", root.name)));
        }
        if let Some(e) = failure {
            state.status = "error".into();
            state.error = Some(e.to_string());
            self.db.set_scan_state(&state).await?;
            self.events.emit(Event::ScanFailed { root_id: root.id.clone(), error: e.to_string() });
            return Err(e);
        }

        summary.removed = self.db.remove_stale(&root.id, "", started).await?;
        state.status = "idle".into();
        state.files_seen = summary.files as i64;
        state.bytes_seen = summary.bytes as i64;
        state.completed_at = Some(now_ms());
        state.error = (summary.unreadable > 0)
            .then(|| format!("{} items could not be read (permissions)", summary.unreadable));
        self.db.set_scan_state(&state).await?;
        self.events.emit(Event::ScanCompleted {
            root_id: root.id.clone(),
            files: summary.files,
            bytes: summary.bytes,
            removed: summary.removed,
            duration_ms: clock.elapsed().as_millis() as u64,
        });
        tracing::info!(root = %root.name, files = summary.files, removed = summary.removed,
            ms = clock.elapsed().as_millis() as u64, "scan completed");
        Ok(summary)
    }

    /// Brings one path (and, for folders, its subtree) up to date.
    pub async fn reindex_path(self: &Arc<Self>, root: &StorageRoot, rel: &str) -> Result<()> {
        let rel = paths::normalize_relative(rel)?;
        if rel.is_empty() {
            self.spawn_scan(root.clone());
            return Ok(());
        }
        if paths::is_hidden_internal(&rel) {
            return Ok(());
        }
        let base = PathBuf::from(&root.absolute_path);
        let full = path_of(&base, &rel);
        if self.is_ignored(&full) {
            return Ok(());
        }
        let now = now_ms();

        match tokio::fs::symlink_metadata(&full).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !base.is_dir() {
                    return Ok(()); // root offline: keep its index intact
                }
                if self.db.delete_subtree(&root.id, &rel).await? > 0 {
                    self.events.emit(Event::FileDeleted { root_id: root.id.clone(), path: rel });
                }
            }
            Err(e) => return Err(e.into()),
            Ok(meta) if meta.file_type().is_symlink() => {
                self.db.delete_subtree(&root.id, &rel).await?;
            }
            Ok(meta) => {
                let existed = self.db.get_file_by_path(&root.id, &rel).await?.is_some();
                self.ensure_ancestors(root, &base, &rel, now).await?;
                self.db.upsert_entries(&root.id, &[entry_from_meta(rel.clone(), &meta)], now).await?;
                if meta.is_dir() {
                    let (tx, mut rx) = mpsc::channel::<Vec<NewEntry>>(4);
                    let ignored = self.ignored.clone();
                    let (walk_base, walk_start) = (base.clone(), full.clone());
                    let walker = tokio::task::spawn_blocking(move || walk(&walk_base, &walk_start, &ignored, &tx));
                    while let Some(batch) = rx.recv().await {
                        self.db.upsert_entries(&root.id, &batch, now).await?;
                    }
                    walker.await.map_err(NexusError::internal)?;
                    self.db.remove_stale(&root.id, &rel, now).await?;
                }
                let root_id = root.id.clone();
                self.events.emit(if existed {
                    Event::FileModified { root_id, path: rel }
                } else {
                    Event::FileCreated { root_id, path: rel }
                });
            }
        }
        Ok(())
    }

    async fn ensure_ancestors(&self, root: &StorageRoot, base: &Path, rel: &str, now: i64) -> Result<()> {
        let parts: Vec<&str> = rel.split('/').collect();
        for depth in 1..parts.len() {
            let ancestor = parts[..depth].join("/");
            if self.db.get_file_by_path(&root.id, &ancestor).await?.is_some() {
                continue;
            }
            if let Ok(meta) = tokio::fs::metadata(path_of(base, &ancestor)).await {
                self.db.upsert_entries(&root.id, &[entry_from_meta(ancestor, &meta)], now).await?;
            }
        }
        Ok(())
    }
}
