//! Backups of a folder here to a paired device, as snapshots.
//!
//! A backup keeps a folder on the device (the store) with:
//!
//! * `objects/<ab>/<blake3>`: every distinct file content, once. A file that
//!   did not change, or appears twice, is not sent again.
//! * `snapshots/<time>_<id>.json`: what the folder held at that time (path,
//!   size, modified time and BLAKE3 of every file).
//!
//! Each run hashes what changed here, sends the contents the store does not
//! have yet, and records a snapshot, here and in the store. Keep rules
//! (the last N, one per day, week and month) decide which snapshots stay;
//! contents no kept snapshot refers to go to the device's trash.
//!
//! A check reads contents back from the device and compares them with
//! their BLAKE3 names; a damaged or missing content is sent again when this
//! computer still has it. After every run a sample is checked, and **Check
//! now** checks everything. A restore copies a whole snapshot, or chosen
//! files and folders from it, beside the original folder or over it (a
//! replaced file keeps its old content as a version).
//!
//! An encrypted backup (see [`super::sealed`]) stores every content and
//! snapshot list encrypted with the backup's key, under names that do not
//! reveal the contents' hashes. The key is kept here protected for this
//! computer's account, and shown once as a recovery key: with it, another
//! computer can open the backup (`POST /snapshots/open`) and restore from it
//! after this one is lost.

use super::audit;
use super::files::{TreeItem, tree_of};
use super::sealed::{self, BackupKey};
use super::transfers::{Meta, Mover, Side, Source, finish_job, peer_target};
use super::uploads::NewUpload;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::peers::Target;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{Datelike, NaiveDate, TimeZone};
use futures_util::StreamExt;
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::types::StorageRoot;
use nexus_common::{NexusError, Result, new_id};
use nexus_database::{NewSnapshotSet, Schedule, Snapshot, SnapshotFile, SnapshotSet};
use nexus_security::Action;
use nexus_storage::paths;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

const TICK: Duration = Duration::from_secs(60);
/// A run that could not reach the device is tried again after this long.
const RETRY_MS: i64 = 15 * 60 * 1000;
/// Bytes read back from the device after each run, to spot damage early.
const SAMPLE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Default)]
pub struct SnapshotControl {
    wake: Notify,
    forced: Mutex<HashSet<String>>,
    checks: Mutex<HashSet<String>>,
    running: Mutex<HashSet<String>>,
}

impl SnapshotControl {
    fn run_now(&self, id: &str) {
        self.forced.lock().unwrap().insert(id.to_string());
        self.wake.notify_one();
    }

    fn check_now(&self, id: &str) {
        self.checks.lock().unwrap().insert(id.to_string());
        self.wake.notify_one();
    }

    fn is_running(&self, id: &str) -> bool {
        self.running.lock().unwrap().contains(id)
    }
}

// ---------------------------------------------------------------------------
// Keep rules (pure)

fn schedule_of(s: &SnapshotSet) -> Schedule {
    Schedule {
        every_hours: s.every_hours,
        keep_last: s.keep_last,
        keep_daily: s.keep_daily,
        keep_weekly: s.keep_weekly,
        keep_monthly: s.keep_monthly,
    }
}

fn check_schedule(k: &Schedule) -> Result<()> {
    if !(1..=24 * 7).contains(&k.every_hours) {
        return Err(NexusError::invalid("Back up between every hour and once a week"));
    }
    if !(1..=1000).contains(&k.keep_last) {
        return Err(NexusError::invalid("Keep at least the latest snapshot"));
    }
    if [k.keep_daily, k.keep_weekly, k.keep_monthly].iter().any(|n| !(0..=1000).contains(n)) {
        return Err(NexusError::invalid("Keep between 0 and 1000 snapshots of each kind"));
    }
    Ok(())
}

/// Which snapshots to keep. `snaps` are (id, taken_at), newest first;
/// `date` gives the calendar day of a time. Keeps the newest `keep_last`,
/// and the newest snapshot of each of the latest `keep_daily` days,
/// `keep_weekly` weeks and `keep_monthly` months that have one.
pub fn keep(snaps: &[(String, i64)], k: &Schedule, date: impl Fn(i64) -> NaiveDate) -> HashSet<String> {
    let mut kept: HashSet<String> = snaps.iter().take(k.keep_last.max(1) as usize).map(|(id, _)| id.clone()).collect();
    let buckets: [(i64, &dyn Fn(NaiveDate) -> i64); 3] = [
        (k.keep_daily, &|d: NaiveDate| d.num_days_from_ce() as i64),
        (k.keep_weekly, &|d: NaiveDate| {
            let w = d.iso_week();
            w.year() as i64 * 100 + w.week() as i64
        }),
        (k.keep_monthly, &|d: NaiveDate| d.year() as i64 * 100 + d.month() as i64),
    ];
    for (count, bucket) in buckets {
        let mut last = None;
        let mut taken = 0;
        for (id, at) in snaps {
            if taken >= count {
                break;
            }
            let b = bucket(date(*at));
            if last != Some(b) {
                last = Some(b);
                taken += 1;
                kept.insert(id.clone());
            }
        }
    }
    kept
}

fn local_date(ms: i64) -> NaiveDate {
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.date_naive()).unwrap_or_default()
}

fn stamp(ms: i64, format: &str) -> String {
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.format(format).to_string()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The store on the device

/// A content in the store.
struct Object {
    id: String,
    size: i64,
}

struct Store<'a> {
    mover: Mover<'a>,
    side: Side,
    root_id: String,
    path: String,
    /// Set for an encrypted backup.
    key: Option<BackupKey>,
}

/// The backup's key, when it is encrypted.
async fn key_of(s: &AppState, set: &SnapshotSet) -> Result<Option<BackupKey>> {
    let Some(protected) = s.db.snapshot_key(&set.id).await? else { return Ok(None) };
    let raw = tokio::task::spawn_blocking(move || nexus_devices::dpapi::unprotect(&protected))
        .await
        .map_err(NexusError::internal)?
        .map_err(|_| NexusError::Unavailable("This computer can no longer read the backup's key. Open the backup again with its recovery key".into()))?;
    BackupKey::from_bytes(&raw).map(Some).ok_or_else(|| NexusError::internal("a backup key of the wrong length"))
}

async fn store_of<'a>(s: &'a AppState, user: &'a AuthUser, set: &SnapshotSet, device: Target) -> Result<Store<'a>> {
    Ok(Store {
        mover: Mover { s, user },
        side: Side::Peer(device),
        root_id: set.remote_root_id.clone(),
        path: set.remote_path.clone(),
        key: key_of(s, set).await?,
    })
}

fn damaged(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof)
}

impl Store<'_> {
    fn objects_dir(&self) -> String {
        paths::join(&self.path, "objects")
    }

    fn snapshots_dir(&self) -> String {
        paths::join(&self.path, "snapshots")
    }

    /// The name a content with this BLAKE3 has in the store: the hash
    /// itself, or for an encrypted backup a keyed hash of it.
    fn name_of(&self, hash: &str) -> String {
        match &self.key {
            Some(k) => k.object_name(hash),
            None => hash.to_string(),
        }
    }

    /// How many bytes a content of `size` bytes takes in the store.
    fn stored_size(&self, size: i64) -> u64 {
        if self.key.is_some() { sealed::sealed_len(size as u64) } else { size as u64 }
    }

    /// Lists a folder of the store; a folder not made yet is empty.
    async fn list(&self, dir: &str) -> Result<Vec<TreeItem>> {
        match self.mover.tree(&self.side, &self.root_id, dir).await {
            Ok(items) => Ok(items),
            Err(NexusError::NotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Contents in the store, by name.
    async fn objects(&self) -> Result<HashMap<String, Object>> {
        Ok(self
            .list(&self.objects_dir())
            .await?
            .into_iter()
            .filter(|i| !i.is_dir)
            .filter_map(|i| {
                let name = paths::split_parent(&i.path).1;
                is_hash(&name).then_some((name, Object { id: i.id, size: i.size }))
            })
            .collect())
    }

    /// Snapshot files in the store: (file id, file name).
    async fn snapshot_files(&self) -> Result<Vec<(String, String)>> {
        Ok(self.list(&self.snapshots_dir()).await?.into_iter().filter(|i| !i.is_dir).map(|i| (i.id, i.path)).collect())
    }

    /// Sends one content from this computer into the store.
    async fn put(&self, job: &Arc<super::transfers::Job>, root: &StorageRoot, base: &str, file: &TreeItem, hash: &str) -> Result<()> {
        let name = self.name_of(hash);
        let req = NewUpload {
            root_id: self.root_id.clone(),
            path: paths::join(&self.objects_dir(), &name[..2]),
            name,
            size: self.stored_size(file.size),
            on_conflict: Some("replace".into()),
            modified_at: None,
            make_parents: true,
        };
        let Some(key) = &self.key else {
            let (_, filename) = paths::split_parent(&file.path);
            let src = Meta {
                id: file.id.clone(),
                root_id: root.id.clone(),
                relative_path: paths::join(base, &file.path),
                filename,
                is_dir: false,
                size: file.size,
                modified_at: file.modified_at,
                content_hash: Some(hash.to_string()),
            };
            return self.mover.send_file(job, &Side::Local, &src, &self.side, req).await.map(|_| ());
        };
        // Encrypted as it is read; what was read is hashed on the way, so a
        // file that changed since it was hashed is not stored under the old
        // hash.
        let path = self.mover.s.storage.resolve(root, &paths::join(base, &file.path))?;
        let read = sealed::PlainHash::default();
        let source = Source::Sealed { path: &path, key: key.content_key(hash), plain: file.size as u64, hash: read.clone() };
        let placed = self.mover.send_from(job, &file.path, &source, &self.side, req).await?;
        if read.finish(file.size as u64).as_deref() != Some(hash) {
            let _ = self.mover.trash(&self.side, &placed.id).await;
            return Err(NexusError::Conflict("The file changed while it was backed up; it is backed up again next time".into()));
        }
        job.update(|i| i.verified += 1);
        Ok(())
    }

    /// Reads a content back: whether it still holds the content with this
    /// BLAKE3 (for an encrypted backup, also that it opens with the key).
    async fn intact(&self, object: &Object, hash: &str) -> Result<bool> {
        let mut hasher = blake3::Hasher::new();
        match &self.key {
            None => {
                let mut stream = self.mover.open(&self.side, &object.id, 0).await?.into_data_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| NexusError::PeerUnreachable(e.to_string()))?;
                    hasher.update(&chunk);
                }
            }
            Some(key) => {
                let Some(plain) = sealed::plain_len(object.size as u64) else { return Ok(false) };
                // The header carries nothing the content needs; the chunks
                // are checked by their tags.
                let body = self.mover.open(&self.side, &object.id, sealed::chunk_offset(0)).await?;
                let stream = sealed::open_stream(body.into_data_stream(), key.content_key(hash), plain, 0, 0);
                futures_util::pin_mut!(stream);
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(c) => hasher.update(&c),
                        Err(e) if damaged(&e) => return Ok(false),
                        Err(e) => return Err(NexusError::PeerUnreachable(e.to_string())),
                    };
                }
            }
        }
        Ok(hasher.finalize().to_hex().as_str() == hash)
    }

    /// Copies a content out of the store to this computer.
    async fn get(&self, job: &Arc<super::transfers::Job>, object: &Object, f: &SnapshotFile, req: NewUpload) -> Result<()> {
        let Some(key) = &self.key else {
            let src = Meta {
                id: object.id.clone(),
                root_id: self.root_id.clone(),
                relative_path: String::new(),
                filename: req.name.clone(),
                is_dir: false,
                size: f.s,
                modified_at: f.m,
                content_hash: Some(f.h.clone()),
            };
            return self.mover.send_file(job, &self.side, &src, &Side::Local, req).await.map(|_| ());
        };
        let source = Source::Unsealed { side: &self.side, id: &object.id, key: key.content_key(&f.h), plain: f.s as u64 };
        let placed = self.mover.send_from(job, &f.p, &source, &Side::Local, req).await?;
        if placed.content_hash.as_deref() != Some(f.h.as_str()) {
            return Err(NexusError::Conflict("The restored file differs from the backup (BLAKE3)".into()));
        }
        job.update(|i| i.verified += 1);
        Ok(())
    }

    /// Reads a snapshot list from the store.
    async fn read_manifest(&self, file_id: &str, snapshot_id: &str) -> Result<StoredManifest> {
        let body = self.mover.open(&self.side, file_id, 0).await?;
        let bytes = axum::body::to_bytes(body, MAX_MANIFEST).await.map_err(|e| NexusError::PeerUnreachable(e.to_string()))?;
        let plain = match &self.key {
            Some(k) => sealed::open_bytes(&k.snapshot_key(snapshot_id), &bytes)
                .map_err(|_| NexusError::invalid("The recovery key does not open this backup"))?,
            None => bytes.to_vec(),
        };
        serde_json::from_slice(&plain).map_err(|e| NexusError::invalid(format!("A snapshot list in the backup could not be read: {e}")))
    }
}

/// Largest snapshot list read back (about two million files).
const MAX_MANIFEST: usize = 512 * 1024 * 1024;

fn is_hash(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The person a set acts for, as they are now.
async fn acting_user(s: &AppState, set: &SnapshotSet) -> Result<AuthUser> {
    let (user, _) = s
        .db
        .find_credentials(&set.created_by)
        .await?
        .ok_or_else(|| NexusError::PermissionDenied(format!("{} no longer has an account here", set.created_by)))?;
    Ok(AuthUser::person(user))
}

/// Hashes a file here that the indexer has not hashed yet.
async fn hash_local(s: &AppState, root: &nexus_common::types::StorageRoot, base: &str, item: &TreeItem) -> Result<String> {
    let path = s.storage.resolve(root, &paths::join(base, &item.path))?;
    let hash = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut file, &mut hasher)?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .map_err(NexusError::internal)??;
    let _ = s.db.set_hash(&item.id, Some(&hash), item.modified_at, now_ms()).await;
    Ok(hash)
}

// ---------------------------------------------------------------------------
// Running a backup

#[derive(Serialize)]
struct ManifestFile<'a> {
    set: &'a str,
    source: String,
    taken_at: i64,
    files: &'a [SnapshotFile],
}

/// A snapshot list as read back from a store.
#[derive(Deserialize)]
struct StoredManifest {
    set: String,
    taken_at: i64,
    files: Vec<SnapshotFile>,
}

async fn run_set(s: &SharedState, set: &SnapshotSet) -> Result<String> {
    let user = acting_user(s, set).await?;
    user.require(Action::Write).map_err(|e| e.0)?;
    user.require_root(&set.local_root_id).map_err(|e| e.0)?;
    let device = peer_target(s, &set.device_id).await?;
    let root = s.db.get_root(&set.local_root_id).await?;
    s.storage.ensure_online(&root)?;
    if s.indexer.is_scanning(&root.id) {
        return Err(NexusError::Unavailable(format!("Waiting for {} to finish indexing", root.name)));
    }
    let store = store_of(s, &user, set, device.clone()).await?;

    let files: Vec<TreeItem> = tree_of(s, &root.id, &set.local_path).await?.into_iter().filter(|i| !i.is_dir).collect();
    let present = store.objects().await?;

    let job = s.jobs.add("backup", format!("Back up {}", set.name), format!("This computer → {}", device.name), user.username.clone());
    let Some(_slot) = s.jobs.start(s, &job).await else { return Ok("Cancelled".into()) };

    // Every file needs its BLAKE3: the indexer has most; the rest are read now.
    let mut hashed: Vec<(TreeItem, String)> = Vec::with_capacity(files.len());
    for file in files {
        if job.is_cancelled() {
            break;
        }
        match file.content_hash.clone() {
            Some(h) => hashed.push((file, h)),
            None => {
                job.update(|i| i.current = Some(format!("Reading {}", file.path)));
                match hash_local(s, &root, &set.local_path, &file).await {
                    Ok(h) => hashed.push((file, h)),
                    Err(e) => job.fail_file(&file.path, &e),
                }
            }
        }
    }

    // Send each content the store does not have, once.
    let mut sending: BTreeMap<&str, &TreeItem> = BTreeMap::new();
    for (file, h) in &hashed {
        if !present.contains_key(&store.name_of(h)) {
            sending.entry(h.as_str()).or_insert(file);
        }
    }
    let new_bytes: u64 = sending.values().map(|f| store.stored_size(f.size)).sum();
    job.update(|i| {
        i.files_total = sending.len() as u64;
        i.bytes_total = new_bytes;
        i.current = None;
    });
    let mut failed: HashSet<String> = HashSet::new();
    for (h, file) in &sending {
        if job.is_cancelled() {
            break;
        }
        if !s.jobs.checkpoint(s, &job).await {
            break;
        }
        job.update(|i| i.current = Some(file.path.clone()));
        match store.put(&job, &root, &set.local_path, file, h).await {
            Ok(()) => job.update(|i| i.files_done += 1),
            Err(_) if job.is_cancelled() => break,
            Err(e) => {
                job.fail_file(&file.path, &e);
                failed.insert(h.to_string());
            }
        }
    }
    if job.is_cancelled() {
        finish_job(s, &job);
        return Ok("Cancelled".into());
    }

    // The snapshot: every file whose content is in the store.
    let mut list: Vec<SnapshotFile> = hashed
        .iter()
        .filter(|(_, h)| !failed.contains(h))
        .map(|(f, h)| SnapshotFile { p: f.path.clone(), s: f.size, m: f.modified_at, h: h.clone() })
        .collect();
    list.sort_by(|a, b| a.p.cmp(&b.p));
    let taken_at = now_ms();
    let snap = Snapshot {
        id: new_id(),
        set_id: set.id.clone(),
        taken_at,
        files: list.len() as i64,
        bytes: list.iter().map(|f| f.s).sum(),
        new_files: (sending.len() - failed.len()) as i64,
        new_bytes: sending.iter().filter(|(h, _)| !failed.contains(**h)).map(|(_, f)| f.size).sum(),
    };
    write_manifest(s, &store, &job, set, &snap, &list).await?;
    s.db.insert_snapshot(&snap, &list).await?;

    let note = prune(s, &store, set).await.unwrap_or_else(|e| format!(" Old snapshots were not cleared: {e}"));
    finish_job(s, &job);
    let errors = job.error_count();
    if errors > 0 {
        return Err(NexusError::Unavailable(format!(
            "Snapshot saved without {errors} file{} that could not be read or sent; they are tried again next time.{note}",
            if errors == 1 { "" } else { "s" }
        )));
    }

    // Read a sample back, to notice damage early.
    let sampled = check_store(s, set, &user, Some(SAMPLE_BYTES)).await;
    let _ = s.db.record_snapshot_check(&set.id, &sampled).await;

    Ok(format!(
        "{} files, {} new{}",
        snap.files,
        snap.new_files,
        note
    ))
}

/// Writes the snapshot's file list into the store.
async fn write_manifest(
    s: &AppState,
    store: &Store<'_>,
    job: &Arc<super::transfers::Job>,
    set: &SnapshotSet,
    snap: &Snapshot,
    list: &[SnapshotFile],
) -> Result<()> {
    let body = serde_json::to_vec(&ManifestFile {
        set: &set.name,
        source: format!("{}: {}", s.node.hostname, set.local_path),
        taken_at: snap.taken_at,
        files: list,
    })
    .map_err(NexusError::internal)?;
    let (body, ext) = match &store.key {
        Some(k) => (sealed::seal_bytes(&k.snapshot_key(&snap.id), &body), "nxb"),
        None => (body, "json"),
    };
    let name = format!("{}_{}.{ext}", stamp(snap.taken_at, "%Y-%m-%d_%H%M%S"), snap.id);
    let tmp = s.data_dir.join(format!("snapshot-{}.{ext}", snap.id));
    tokio::fs::write(&tmp, &body).await?;
    let req = NewUpload {
        root_id: store.root_id.clone(),
        path: store.snapshots_dir(),
        name,
        size: body.len() as u64,
        on_conflict: Some("replace".into()),
        modified_at: Some(snap.taken_at),
        make_parents: true,
    };
    let before = job.progress().0;
    let sent = store.mover.send_disk_file(job, &tmp, &store.side, req).await;
    job.set_bytes(before);
    let _ = tokio::fs::remove_file(&tmp).await;
    sent.map(|_| ())
}

/// Applies the keep rules: forgets old snapshots, and moves contents no
/// kept snapshot uses to the device's trash. Returns a note for the result.
async fn prune(s: &AppState, store: &Store<'_>, set: &SnapshotSet) -> Result<String> {
    let snaps = s.db.list_snapshots(&set.id).await?;
    let ids: Vec<(String, i64)> = snaps.iter().map(|x| (x.id.clone(), x.taken_at)).collect();
    let kept = keep(&ids, &schedule_of(set), local_date);
    let files = store.snapshot_files().await?;
    let mut forgot = 0;
    for snap in snaps.iter().filter(|x| !kept.contains(&x.id)) {
        s.db.delete_snapshot(&snap.id).await?;
        for (file_id, _) in files.iter().filter(|(_, name)| name.contains(&snap.id)) {
            let _ = store.mover.trash(&store.side, file_id).await;
        }
        forgot += 1;
    }
    // Snapshots in the store this computer does not know (an earlier setup
    // wrote into the same folder): their contents must stay.
    let known: HashSet<String> = s.db.list_snapshots(&set.id).await?.into_iter().map(|x| x.id).collect();
    let pruned: HashSet<&str> = snaps.iter().filter(|x| !kept.contains(&x.id)).map(|x| x.id.as_str()).collect();
    let foreign = files.iter().any(|(_, name)| !known.iter().any(|id| name.contains(id.as_str())) && !pruned.iter().any(|id| name.contains(id)));
    if foreign {
        return Ok(if forgot > 0 { format!(", {forgot} old snapshots removed") } else { String::new() });
    }
    let used: HashSet<String> = s.db.snapshot_hashes(&set.id).await?.iter().map(|h| store.name_of(h)).collect();
    let mut freed = 0i64;
    for (name, object) in store.objects().await? {
        if !used.contains(&name) && store.mover.trash(&store.side, &object.id).await.is_ok() {
            freed += object.size;
        }
    }
    Ok(match (forgot, freed) {
        (0, 0) => String::new(),
        (n, 0) => format!(", {n} old snapshot{} removed", if n == 1 { "" } else { "s" }),
        (n, b) => format!(", {n} old snapshot{} removed ({} moved to the trash on the device)", if n == 1 { "" } else { "s" }, human_bytes(b)),
    })
}

fn human_bytes(b: i64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

// ---------------------------------------------------------------------------
// Checking

/// Reads contents back from the store and compares them with their names;
/// sends again what is damaged or missing when this computer still has it.
/// `budget` limits how many bytes are read (a sample); `None` reads all.
async fn check_store(s: &SharedState, set: &SnapshotSet, user: &AuthUser, budget: Option<u64>) -> String {
    match check_inner(s, set, user, budget).await {
        Ok(text) => text,
        Err(e) => format!("Could not check: {e}"),
    }
}

async fn check_inner(s: &SharedState, set: &SnapshotSet, user: &AuthUser, budget: Option<u64>) -> Result<String> {
    let device = peer_target(s, &set.device_id).await?;
    let store = store_of(s, user, set, device.clone()).await?;
    let used = s.db.snapshot_hashes(&set.id).await?;
    let present = store.objects().await?;
    let mut missing: Vec<&String> = Vec::new();
    // Which to read: all, or a rotating sample.
    let mut order: Vec<(&String, &Object)> = Vec::new();
    for h in &used {
        match present.get(&store.name_of(h)) {
            Some(object) => order.push((h, object)),
            None => missing.push(h),
        }
    }
    order.sort_by_key(|(h, _)| *h);
    if budget.is_some() {
        let start = (now_ms() / 1000) as usize % order.len().max(1);
        order.rotate_left(start);
    }
    let full = budget.is_none();
    let job = full.then(|| s.jobs.add("backup", format!("Check {}", set.name), format!("{} → this computer", device.name), user.username.clone()));
    let _slot = match &job {
        Some(job) => match s.jobs.start(s, job).await {
            Some(slot) => Some(slot),
            None => return Ok("Check cancelled".into()),
        },
        None => None,
    };
    if let Some(job) = &job {
        job.update(|i| {
            i.files_total = order.len() as u64;
            i.bytes_total = order.iter().map(|(_, o)| o.size as u64).sum();
        });
    }
    let mut read = 0u64;
    let mut checked = 0usize;
    let mut damaged: Vec<String> = Vec::new();
    for (hash, object) in &order {
        if let Some(limit) = budget
            && read >= limit
        {
            break;
        }
        if job.as_ref().is_some_and(|j| j.is_cancelled()) {
            break;
        }
        if !store.intact(object, hash).await? {
            damaged.push((*hash).clone());
        }
        read += object.size as u64;
        checked += 1;
        if let Some(job) = &job {
            job.update(|i| i.files_done += 1);
            job.set_bytes(read);
        }
    }

    // Send again what is wrong, from files here with the same content.
    let mut repaired = 0;
    let mut lost = 0;
    let bad: Vec<String> = damaged.iter().cloned().chain(missing.iter().map(|h| (*h).clone())).collect();
    if !bad.is_empty() {
        let root = s.db.get_root(&set.local_root_id).await?;
        let here: HashMap<String, TreeItem> = tree_of(s, &root.id, &set.local_path)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|i| Some((i.content_hash.clone()?, i)))
            .collect();
        let fix = s.jobs.add("backup", format!("Repair {}", set.name), format!("This computer → {}", device.name), user.username.clone());
        if let Some(_slot) = s.jobs.start(s, &fix).await {
            for hash in &bad {
                match here.get(hash) {
                    Some(file) if store.put(&fix, &root, &set.local_path, file, hash).await.is_ok() => repaired += 1,
                    _ => lost += 1,
                }
            }
            finish_job(s, &fix);
        }
    }
    if let Some(job) = &job {
        finish_job(s, job);
    }

    let what = if full { format!("All {checked} contents read back") } else { format!("{checked} contents read back") };
    Ok(match (damaged.len() + missing.len(), lost) {
        (0, _) => format!("{what}: intact"),
        (_, 0) => format!("{what}: {} damaged and {} missing, all sent again", damaged.len(), missing.len()),
        _ => format!(
            "{what}: {} damaged and {} missing; {repaired} sent again, {lost} no longer here (files in older snapshots may not restore)",
            damaged.len(),
            missing.len()
        ),
    })
}

// ---------------------------------------------------------------------------
// Restoring

/// Copies files of a snapshot back here.
async fn restore(s: &SharedState, user: &AuthUser, job: &Arc<super::transfers::Job>, set: &SnapshotSet, files: Vec<SnapshotFile>, dest: String, over: bool) {
    let Some(_slot) = s.jobs.start(s, job).await else { return };
    let outcome: Result<()> = async {
        let device = peer_target(s, &set.device_id).await?;
        let store = store_of(s, user, set, device).await?;
        let present = store.objects().await?;
        // Over the original: files that already hold the content stay.
        let current: HashMap<String, String> = if over {
            tree_of(s, &set.local_root_id, &dest)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter_map(|i| Some((i.path.to_lowercase(), i.content_hash?)))
                .collect()
        } else {
            HashMap::new()
        };
        let todo: Vec<&SnapshotFile> = files.iter().filter(|f| current.get(&f.p.to_lowercase()) != Some(&f.h)).collect();
        job.update(|i| {
            i.files_total = todo.len() as u64;
            i.bytes_total = todo.iter().map(|f| f.s as u64).sum();
        });
        for f in todo {
            if job.is_cancelled() || !s.jobs.checkpoint(s, job).await {
                break;
            }
            let (parent, name) = paths::split_parent(&f.p);
            job.update(|i| i.current = Some(f.p.clone()));
            let Some(object) = present.get(&store.name_of(&f.h)) else {
                job.fail_file(&f.p, &NexusError::NotFound("Its content in the backup".into()));
                continue;
            };
            let req = NewUpload {
                root_id: set.local_root_id.clone(),
                path: paths::join(&dest, &parent),
                name,
                size: f.s as u64,
                on_conflict: Some(if over { "replace" } else { "rename" }.into()),
                modified_at: f.m,
                make_parents: true,
            };
            match store.get(job, object, f, req).await {
                Ok(()) => job.update(|i| i.files_done += 1),
                Err(_) if job.is_cancelled() => break,
                Err(e) => job.fail_file(&f.p, &e),
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = outcome {
        job.fail_file(&set.name, &e);
    }
    finish_job(s, job);
    audit(s, user, "backup.restored", Some(&format!("{} → {}", set.name, if dest.is_empty() { "/" } else { &dest }))).await;
}

// ---------------------------------------------------------------------------
// Scheduling

async fn run_and_record(s: &SharedState, set: &SnapshotSet) {
    s.snapshots.running.lock().unwrap().insert(set.id.clone());
    s.events.emit(Event::SnapshotsUpdated { id: set.id.clone() });
    let result = run_set(s, set).await;
    let (ok, text, error) = match &result {
        Ok(text) => (true, text.clone(), None),
        Err(NexusError::PeerUnreachable(_)) => (false, "Waiting for the device".to_string(), Some(result.as_ref().unwrap_err().to_string())),
        Err(e) => (false, "Needs attention".to_string(), Some(e.to_string())),
    };
    if let Err(e) = s.db.record_snapshot_run(&set.id, ok, &text, error.as_deref()).await {
        tracing::warn!(error = %e, "could not record a backup run");
    }
    s.snapshots.running.lock().unwrap().remove(&set.id);
    s.events.emit(Event::SnapshotsUpdated { id: set.id.clone() });
}

async fn check_and_record(s: &SharedState, set: &SnapshotSet) {
    s.snapshots.running.lock().unwrap().insert(set.id.clone());
    s.events.emit(Event::SnapshotsUpdated { id: set.id.clone() });
    let text = match acting_user(s, set).await {
        Ok(user) => check_store(s, set, &user, None).await,
        Err(e) => format!("Could not check: {e}"),
    };
    let _ = s.db.record_snapshot_check(&set.id, &text).await;
    s.snapshots.running.lock().unwrap().remove(&set.id);
    s.events.emit(Event::SnapshotsUpdated { id: set.id.clone() });
}

/// Runs backups when they are due or asked for.
pub fn spawn(s: SharedState) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = s.snapshots.wake.notified() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
            let Ok(sets) = s.db.list_snapshot_sets().await else { continue };
            let now = now_ms();
            for set in sets {
                if s.snapshots.checks.lock().unwrap().remove(&set.id) {
                    check_and_record(&s, &set).await;
                }
                let forced = s.snapshots.forced.lock().unwrap().remove(&set.id);
                if !set.enabled && !forced {
                    continue;
                }
                let last = set.last_run_at.unwrap_or(0);
                let failed = set.last_ok_at.unwrap_or(0) < last;
                let due = now - last >= set.every_hours * 3_600_000 || (failed && now - last >= RETRY_MS);
                if forced || due {
                    run_and_record(&s, &set).await;
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Routes

#[derive(Serialize)]
pub struct SetView {
    #[serde(flatten)]
    set: SnapshotSet,
    device_name: String,
    local_label: String,
    running: bool,
    snapshots: usize,
    latest: Option<Snapshot>,
}

async fn view(s: &AppState, set: SnapshotSet) -> Result<SetView> {
    let device_name = s.db.get_device(&set.device_id).await.map(|d| d.name).unwrap_or_else(|_| "Removed device".into());
    let root = s.db.get_root(&set.local_root_id).await.map(|r| r.name).unwrap_or_else(|_| "Removed storage".into());
    let local_label = if set.local_path.is_empty() { root } else { format!("{root} / {}", set.local_path) };
    let snaps = s.db.list_snapshots(&set.id).await?;
    Ok(SetView {
        running: s.snapshots.is_running(&set.id),
        snapshots: snaps.len(),
        latest: snaps.into_iter().next(),
        device_name,
        local_label,
        set,
    })
}

/// A set the caller may see (its folder here) and, with `change`, change:
/// their own, or any if they manage people.
async fn find_set(s: &AppState, user: &AuthUser, id: &str, change: bool) -> ApiResult<SnapshotSet> {
    user.require_person()?;
    let set = s.db.get_snapshot_set(id).await?;
    if !user.sees_root(&set.local_root_id) {
        return Err(NexusError::NotFound("Backup".into()).into());
    }
    if change {
        user.require(Action::Write)?;
        if set.created_by != user.username && !nexus_security::role_allows(user.role, Action::ManageUsers) {
            return Err(NexusError::PermissionDenied(format!("Only {} or an administrator can change this backup", set.created_by)).into());
        }
    }
    Ok(set)
}

/// `GET /snapshots`
pub async fn list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<SetView>>> {
    user.require_person()?;
    user.require(Action::Read)?;
    let mut out = Vec::new();
    for set in s.db.list_snapshot_sets().await?.into_iter().filter(|x| user.sees_root(&x.local_root_id)) {
        out.push(view(&s, set).await?);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateSet {
    local_root_id: String,
    #[serde(default)]
    local_path: String,
    device_id: String,
    remote_root_id: String,
    /// The folder on the device the backup goes into (a folder is made in it).
    #[serde(default)]
    remote_path: String,
    schedule: Option<Schedule>,
    /// Encrypt what is stored on the device (the default).
    #[serde(default = "yes")]
    encrypt: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize)]
pub struct CreatedSet {
    #[serde(flatten)]
    view: SetView,
    /// For an encrypted backup: its recovery key, shown this once.
    recovery_key: Option<String>,
}

/// Keeps a backup's key here, protected for this computer's account.
async fn keep_key(s: &AppState, set_id: &str, key: &BackupKey) -> Result<()> {
    let raw = key.bytes().to_vec();
    let protected = tokio::task::spawn_blocking(move || nexus_devices::dpapi::protect(&raw)).await.map_err(NexusError::internal)??;
    s.db.set_snapshot_key(set_id, &protected).await
}

fn file_name_safe(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '-' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() { "Backup".into() } else { cleaned.chars().take(80).collect() }
}

/// `POST /snapshots`
pub async fn create(State(s): State<SharedState>, user: AuthUser, Json(req): Json<CreateSet>) -> ApiResult<(StatusCode, Json<CreatedSet>)> {
    user.require_person()?;
    user.require(Action::Write)?;
    if user.user_id.is_none() {
        return Err(NexusError::PermissionDenied("Sign in with an account to set up backups".into()).into());
    }
    user.require_root(&req.local_root_id)?;
    let schedule = req.schedule.unwrap_or(Schedule { every_hours: 24, keep_last: 7, keep_daily: 14, keep_weekly: 8, keep_monthly: 12 });
    check_schedule(&schedule)?;
    let root = s.db.get_root(&req.local_root_id).await?;
    let local_path = paths::normalize_relative(&req.local_path)?;
    tree_of(&s, &root.id, &local_path).await.map_err(|_| NexusError::NotFound("The folder here".into()))?;
    let name = if local_path.is_empty() { root.name.clone() } else { paths::split_parent(&local_path).1 };

    // Make the store's folder on the device: "<computer> - <folder>".
    let device = peer_target(&s, &req.device_id).await?;
    let parent = paths::normalize_relative(&req.remote_path)?;
    let mover = Mover { s: &s, user: &user };
    let side = Side::Peer(device.clone());
    let this = s.db.get_device(&s.node.device_id).await.map(|d| d.name).unwrap_or_else(|_| s.node.hostname.clone());
    let folder = mover.mkdir_free(&side, &req.remote_root_id, &parent, &file_name_safe(&format!("{this} - {name}"))).await?;
    let remote_path = paths::join(&parent, &folder);
    let remote_label = format!("{} / {remote_path}", device.name);

    let set = s
        .db
        .create_snapshot_set(&NewSnapshotSet {
            name: &name,
            local_root_id: &root.id,
            local_path: &local_path,
            device_id: &req.device_id,
            remote_root_id: &req.remote_root_id,
            remote_path: &remote_path,
            remote_label: &remote_label,
            created_by: &user.username,
        })
        .await?;
    let key = if req.encrypt { Some(BackupKey::generate()?) } else { None };
    if let Some(key) = &key
        && let Err(e) = keep_key(&s, &set.id, key).await
    {
        let _ = s.db.delete_snapshot_set(&set.id).await;
        return Err(e.into());
    }
    let set = s.db.update_snapshot_set(&set.id, None, Some(schedule)).await?;
    audit(&s, &user, "backup.created", Some(&format!("{name} → {remote_label}"))).await;
    s.snapshots.run_now(&set.id);
    let recovery_key = key.map(|k| k.recovery_key());
    Ok((StatusCode::CREATED, Json(CreatedSet { view: view(&s, set).await?, recovery_key })))
}

#[derive(Deserialize)]
pub struct ShowKey {
    password: String,
}

#[derive(Serialize)]
pub struct RecoveryKey {
    recovery_key: String,
}

/// `POST /snapshots/{id}/recovery-key`: shows an encrypted backup's
/// recovery key again, after the person enters their password.
pub async fn recovery_key(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>, Json(req): Json<ShowKey>) -> ApiResult<Json<RecoveryKey>> {
    let set = find_set(&s, &user, &id, true).await?;
    let found = s.db.find_credentials(&user.username).await?;
    let password = req.password;
    let valid = tokio::task::spawn_blocking(move || found.is_some_and(|(_, hash)| nexus_security::verify_password(&password, &hash)))
        .await
        .map_err(NexusError::internal)?;
    if !valid {
        tokio::time::sleep(Duration::from_millis(400)).await;
        return Err(NexusError::PermissionDenied("That password is not right".into()).into());
    }
    let key = key_of(&s, &set).await?.ok_or_else(|| NexusError::invalid("This backup is not encrypted"))?;
    audit(&s, &user, "backup.key_shown", Some(&set.name)).await;
    Ok(Json(RecoveryKey { recovery_key: key.recovery_key() }))
}

#[derive(Deserialize)]
pub struct OpenSet {
    /// Where the backup is: a device, a storage location on it and the
    /// backup's folder.
    device_id: String,
    remote_root_id: String,
    remote_path: String,
    /// Needed when the backup is encrypted.
    #[serde(default)]
    recovery_key: String,
    /// Where restores go here: "over the original" means this folder.
    local_root_id: String,
    #[serde(default)]
    local_path: String,
}

/// The snapshot id in a snapshot list's file name (`<time>_<id>.json`).
fn snapshot_id_of(file_name: &str) -> Option<(&str, bool)> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    let encrypted = match ext {
        "nxb" => true,
        "json" => false,
        _ => return None,
    };
    let id = stem.rsplit_once('_')?.1;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then_some((id, encrypted))
}

/// `POST /snapshots/open`: opens a backup another computer made (or one
/// this computer made before it was set up again), to browse and restore
/// it here. It is added paused; turning it on continues it from here.
pub async fn open(State(s): State<SharedState>, user: AuthUser, Json(req): Json<OpenSet>) -> ApiResult<(StatusCode, Json<SetView>)> {
    user.require_person()?;
    user.require(Action::Write)?;
    if user.user_id.is_none() {
        return Err(NexusError::PermissionDenied("Sign in with an account to open backups".into()).into());
    }
    user.require_root(&req.local_root_id)?;
    let root = s.db.get_root(&req.local_root_id).await?;
    let local_path = paths::normalize_relative(&req.local_path)?;
    let remote_path = paths::normalize_relative(&req.remote_path)?;
    if remote_path.is_empty() {
        return Err(NexusError::invalid("Choose the backup's folder on the device").into());
    }
    let opened = s.db.list_snapshot_sets().await?.into_iter().find(|x| {
        x.device_id == req.device_id && x.remote_root_id == req.remote_root_id && x.remote_path.eq_ignore_ascii_case(&remote_path)
    });
    if let Some(x) = opened {
        return Err(NexusError::Conflict(format!("This backup is already on the Backups page, as {}", x.name)).into());
    }

    let device = peer_target(&s, &req.device_id).await?;
    let mut store = Store {
        mover: Mover { s: &s, user: &user },
        side: Side::Peer(device.clone()),
        root_id: req.remote_root_id.clone(),
        path: remote_path.clone(),
        key: None,
    };
    let lists: Vec<(String, String, bool)> = store
        .snapshot_files()
        .await?
        .into_iter()
        .filter_map(|(file_id, path)| {
            let name = paths::split_parent(&path).1;
            let (id, encrypted) = snapshot_id_of(&name)?;
            Some((file_id, id.to_string(), encrypted))
        })
        .collect();
    if lists.is_empty() {
        return Err(NexusError::NotFound("A backup in this folder (it has no snapshots)".into()).into());
    }
    let encrypted = lists.iter().any(|(_, _, e)| *e);
    if encrypted {
        if req.recovery_key.trim().is_empty() {
            return Err(NexusError::invalid("This backup is encrypted. Enter its recovery key").into());
        }
        store.key = Some(
            BackupKey::from_recovery_key(&req.recovery_key)
                .ok_or_else(|| NexusError::invalid("That is not a valid recovery key. Check it for typing mistakes"))?,
        );
    }

    // Read every snapshot list; the newest names the backup.
    let mut snaps: Vec<(Snapshot, Vec<SnapshotFile>)> = Vec::new();
    let mut name = None;
    for (file_id, id, _) in lists.iter().filter(|(_, _, e)| *e == encrypted) {
        let m = store.read_manifest(file_id, id).await?;
        if name.as_ref().is_none_or(|(at, _)| m.taken_at > *at) {
            name = Some((m.taken_at, m.set.clone()));
        }
        let snap = Snapshot {
            id: id.clone(),
            set_id: String::new(),
            taken_at: m.taken_at,
            files: m.files.len() as i64,
            bytes: m.files.iter().map(|f| f.s).sum(),
            new_files: 0,
            new_bytes: 0,
        };
        snaps.push((snap, m.files));
    }
    let name = name.map(|(_, n)| n).unwrap_or_else(|| paths::split_parent(&remote_path).1);

    // Where restores go here; made if it is not there.
    if !local_path.is_empty() && tree_of(&s, &root.id, &local_path).await.is_err() {
        let (parent, folder) = paths::split_parent(&local_path);
        Mover { s: &s, user: &user }.mkdir(&Side::Local, &root.id, &parent, &folder).await?;
    }
    let set = s
        .db
        .create_snapshot_set(&NewSnapshotSet {
            name: &name,
            local_root_id: &root.id,
            local_path: &local_path,
            device_id: &req.device_id,
            remote_root_id: &req.remote_root_id,
            remote_path: &remote_path,
            remote_label: &format!("{} / {remote_path}", device.name),
            created_by: &user.username,
        })
        .await?;
    let outcome: Result<()> = async {
        s.db.update_snapshot_set(&set.id, Some(false), None).await?;
        if let Some(key) = &store.key {
            keep_key(&s, &set.id, key).await?;
        }
        for (mut snap, files) in snaps {
            snap.set_id = set.id.clone();
            s.db.insert_snapshot(&snap, &files).await?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = outcome {
        let _ = s.db.delete_snapshot_set(&set.id).await;
        return Err(e.into());
    }
    let set = s.db.get_snapshot_set(&set.id).await?;
    audit(&s, &user, "backup.opened", Some(&format!("{name} on {}", device.name))).await;
    s.events.emit(Event::SnapshotsUpdated { id: set.id.clone() });
    Ok((StatusCode::CREATED, Json(view(&s, set).await?)))
}

#[derive(Deserialize)]
pub struct UpdateSet {
    enabled: Option<bool>,
    schedule: Option<Schedule>,
}

/// `PATCH /snapshots/{id}`
pub async fn update(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>, Json(req): Json<UpdateSet>) -> ApiResult<Json<SetView>> {
    find_set(&s, &user, &id, true).await?;
    if let Some(k) = &req.schedule {
        check_schedule(k)?;
    }
    let set = s.db.update_snapshot_set(&id, req.enabled, req.schedule).await?;
    audit(&s, &user, "backup.changed", Some(&set.name)).await;
    s.events.emit(Event::SnapshotsUpdated { id: id.clone() });
    Ok(Json(view(&s, set).await?))
}

/// `DELETE /snapshots/{id}`: stops the backup. What is on the device stays.
pub async fn remove(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let set = find_set(&s, &user, &id, true).await?;
    s.db.delete_snapshot_set(&id).await?;
    audit(&s, &user, "backup.removed", Some(&set.name)).await;
    s.events.emit(Event::SnapshotsUpdated { id });
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /snapshots/{id}/run`
pub async fn run(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    find_set(&s, &user, &id, true).await?;
    s.snapshots.run_now(&id);
    Ok(StatusCode::ACCEPTED)
}

/// `POST /snapshots/{id}/check`
pub async fn check(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    find_set(&s, &user, &id, true).await?;
    s.snapshots.check_now(&id);
    Ok(StatusCode::ACCEPTED)
}

/// `GET /snapshots/{id}/history`
pub async fn history(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<Vec<Snapshot>>> {
    find_set(&s, &user, &id, false).await?;
    Ok(Json(s.db.list_snapshots(&id).await?))
}

#[derive(Deserialize)]
pub struct BrowseQuery {
    #[serde(default)]
    path: String,
}

#[derive(Serialize)]
pub struct SnapshotEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: i64,
    modified_at: Option<i64>,
    /// Files inside a folder.
    files: usize,
}

/// What a snapshot held directly in a folder.
pub fn browse(list: &[SnapshotFile], folder: &str) -> Vec<SnapshotEntry> {
    let prefix = if folder.is_empty() { String::new() } else { format!("{}/", folder.to_lowercase()) };
    let mut dirs: BTreeMap<String, SnapshotEntry> = BTreeMap::new();
    let mut files = Vec::new();
    for f in list {
        let lower = f.p.to_lowercase();
        let Some(rest) = lower.strip_prefix(&prefix) else { continue };
        // Show the names as they are spelled, unless lowercasing changed lengths.
        let rest_real = if f.p.len() == lower.len() { &f.p[prefix.len()..] } else { rest };
        match rest_real.split_once('/') {
            Some((dir, _)) => {
                let e = dirs.entry(dir.to_lowercase()).or_insert_with(|| SnapshotEntry {
                    name: dir.to_string(),
                    path: paths::join(folder, dir),
                    is_dir: true,
                    size: 0,
                    modified_at: None,
                    files: 0,
                });
                e.size += f.s;
                e.files += 1;
                e.modified_at = e.modified_at.max(f.m);
            }
            None => files.push(SnapshotEntry { name: rest_real.to_string(), path: f.p.clone(), is_dir: false, size: f.s, modified_at: f.m, files: 0 }),
        }
    }
    dirs.into_values().chain(files).collect()
}

/// `GET /snapshots/{id}/{snapshot}/files?path=`
pub async fn files(
    State(s): State<SharedState>,
    user: AuthUser,
    Path((id, snap)): Path<(String, String)>,
    Query(q): Query<BrowseQuery>,
) -> ApiResult<Json<Vec<SnapshotEntry>>> {
    find_set(&s, &user, &id, false).await?;
    let (_, list) = s.db.get_snapshot(&id, &snap).await?;
    Ok(Json(browse(&list, &paths::normalize_relative(&q.path)?)))
}

#[derive(Deserialize)]
pub struct RestoreRequest {
    /// Files and folders of the snapshot; empty for all of it.
    #[serde(default)]
    paths: Vec<String>,
    /// `beside` (a new folder next to the original) or `original` (over it).
    #[serde(default)]
    to: String,
}

#[derive(Serialize)]
pub struct RestoreStarted {
    job_id: String,
    folder: String,
}

/// `POST /snapshots/{id}/{snapshot}/restore`
pub async fn start_restore(
    State(s): State<SharedState>,
    user: AuthUser,
    Path((id, snap)): Path<(String, String)>,
    Json(req): Json<RestoreRequest>,
) -> ApiResult<(StatusCode, Json<RestoreStarted>)> {
    let set = find_set(&s, &user, &id, true).await?;
    user.require_root(&set.local_root_id)?;
    let (snapshot, list) = s.db.get_snapshot(&id, &snap).await?;
    let chosen: Vec<String> = req.paths.iter().map(|p| paths::normalize_relative(p)).collect::<Result<_>>()?;
    let files: Vec<SnapshotFile> = list
        .into_iter()
        .filter(|f| {
            chosen.is_empty() || {
                let p = f.p.to_lowercase();
                chosen.iter().any(|c| {
                    let c = c.to_lowercase();
                    c.is_empty() || p == c || p.starts_with(&format!("{c}/"))
                })
            }
        })
        .collect();
    if files.is_empty() {
        return Err(NexusError::invalid("Nothing to restore").into());
    }
    let over = match req.to.as_str() {
        "original" => true,
        "beside" | "" => false,
        _ => return Err(NexusError::invalid("Restore beside the original folder or over it").into()),
    };
    let dest = if over {
        set.local_path.clone()
    } else {
        let (parent, name) = paths::split_parent(&set.local_path);
        let label = format!("{} (restored {})", if name.is_empty() { &set.name } else { &name }, stamp(snapshot.taken_at, "%Y-%m-%d %H%M"));
        let made = Mover { s: &s, user: &user }.mkdir_free(&Side::Local, &set.local_root_id, &parent, &file_name_safe(&label)).await?;
        paths::join(&parent, &made)
    };
    let job = s.jobs.add("backup", format!("Restore {}", set.name), "Backup → this computer".into(), user.username.clone());
    let started = RestoreStarted { job_id: job.id.clone(), folder: dest.clone() };
    let state = s.clone();
    tokio::spawn(async move { restore(&state, &user, &job, &set, files, dest, over).await });
    Ok((StatusCode::ACCEPTED, Json(started)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(ms_day: i64) -> NaiveDate {
        NaiveDate::from_num_days_from_ce_opt(738_000 + ms_day as i32).unwrap()
    }

    fn schedule(last: i64, daily: i64, weekly: i64, monthly: i64) -> Schedule {
        Schedule { every_hours: 24, keep_last: last, keep_daily: daily, keep_weekly: weekly, keep_monthly: monthly }
    }

    /// Two snapshots a day for 100 days, newest first; `taken_at` counts days.
    fn history() -> Vec<(String, i64)> {
        (0..200).rev().map(|i| (format!("s{i}"), i / 2)).collect()
    }

    #[test]
    fn keeps_the_latest_and_one_per_day() {
        let kept = keep(&history(), &schedule(3, 5, 0, 0), day);
        // The latest three, then the newest of each of the 5 latest days
        // (which already includes s199 and s197).
        for id in ["s199", "s198", "s197", "s195", "s193", "s191"] {
            assert!(kept.contains(id), "{id}");
        }
        assert_eq!(kept.len(), 6);
    }

    #[test]
    fn weekly_and_monthly_reach_further_back() {
        let snaps = history();
        let kept = keep(&snaps, &schedule(1, 0, 4, 3), day);
        let oldest_kept = kept.iter().map(|id| id[1..].parse::<i64>().unwrap()).min().unwrap();
        // Three months go back further than four weeks.
        assert!(oldest_kept < 199 - 2 * 28, "{oldest_kept}");
        assert!(kept.len() <= 1 + 4 + 3);
    }

    #[test]
    fn keeps_at_least_one() {
        assert_eq!(keep(&history(), &schedule(0, 0, 0, 0), day).len(), 1);
        assert!(keep(&[], &schedule(1, 1, 1, 1), day).is_empty());
    }

    #[test]
    fn browsing_a_snapshot() {
        let f = |p: &str, s: i64| SnapshotFile { p: p.into(), s, m: Some(s), h: "x".into() };
        let list = vec![f("a.txt", 1), f("Docs/b.txt", 2), f("Docs/Old/c.txt", 3), f("docs/d.txt", 4)];
        let top = browse(&list, "");
        assert_eq!(top.len(), 2);
        assert!(top[0].is_dir && top[0].name == "Docs" && top[0].files == 3 && top[0].size == 9);
        assert_eq!(top[1].name, "a.txt");
        let docs = browse(&list, "Docs");
        assert_eq!(docs.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["Old", "b.txt", "d.txt"]);
    }

    #[test]
    fn schedules_are_checked() {
        assert!(check_schedule(&schedule(7, 14, 8, 12)).is_ok());
        assert!(check_schedule(&schedule(0, 14, 8, 12)).is_err());
        assert!(check_schedule(&Schedule { every_hours: 0, ..schedule(1, 0, 0, 0) }).is_err());
        assert!(check_schedule(&schedule(1, -1, 0, 0)).is_err());
    }

    #[test]
    fn snapshot_list_names() {
        let id = new_id();
        assert_eq!(snapshot_id_of(&format!("2026-11-18_101500_{id}.nxb")), Some((id.as_str(), true)));
        assert_eq!(snapshot_id_of(&format!("2026-11-18_101500_{id}.json")), Some((id.as_str(), false)));
        assert_eq!(snapshot_id_of("notes.txt"), None);
        assert_eq!(snapshot_id_of("2026_x y.json"), None);
    }

    #[test]
    fn store_names() {
        assert!(is_hash(&"ab".repeat(32)));
        assert!(!is_hash("snapshot.json"));
        assert_eq!(file_name_safe("PC: Docs/Old?"), "PC- Docs-Old-");
        assert_eq!(file_name_safe("  "), "Backup");
    }
}
