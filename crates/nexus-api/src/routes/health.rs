//! Drive health.
//!
//! * **Disks**: what Windows reports about each physical disk (healthy,
//!   warning or failing; temperature, wear and errors where it allows),
//!   read every half hour.
//! * **Presence**: every storage location is looked at each minute; when
//!   one disappears (a drive unplugged or failing) or comes back, it is
//!   noted in the activity log and shown until it is back.
//! * **Free space**: locations whose drive is nearly full.
//! * **Verify files**: a full read of a storage location, every 30 days and
//!   on request, comparing each file with the hash taken when it was
//!   indexed. A file whose size and date are unchanged but whose content is
//!   not was damaged on the disk ("bit rot"); such files are listed and can
//!   be repaired from an extra copy or a backup that holds the right content.
//!   The damaged content is kept as a version, so a repair can be undone.

use super::audit;
use super::transfers::{Meta, Mover, Side, finish_job, peer_target};
use super::uploads::NewUpload;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use nexus_common::events::Event;
use nexus_common::time::{now_ms, system_time_ms};
use nexus_common::types::StorageRoot;
use nexus_common::{NexusError, Result};
use nexus_database::{DamagedFile, RootCheck, SyncPair};
use nexus_security::Action;
use nexus_storage::StorageManager;
use nexus_storage::drives::{self, DriveInfo};
use nexus_storage::health::{self, PhysicalDisk};
use nexus_storage::paths;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Notify;

const TICK: Duration = Duration::from_secs(60);
const DISKS_EVERY_MS: i64 = 30 * 60 * 1000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// Each storage location is read in full this often.
const VERIFY_EVERY_MS: i64 = 30 * DAY_MS;
/// Wait this long after start-up before reading disks in full.
const VERIFY_DELAY_MS: i64 = 10 * 60 * 1000;
const BATCH: i64 = 256;

/// When the disks were read, and what Windows said (or why it did not).
type DiskReading = (i64, std::result::Result<Vec<PhysicalDisk>, String>);

#[derive(Default)]
pub struct HealthControl {
    wake: Notify,
    /// When the disks were last read, and what came back.
    disks: Mutex<Option<DiskReading>>,
    refresh_disks: Mutex<bool>,
    /// Storage locations being read in full now.
    verifying: Mutex<HashSet<String>>,
    /// Storage locations someone asked to read now, and who asked.
    verify_now: Mutex<HashMap<String, String>>,
}

impl HealthControl {
    fn is_verifying(&self, root_id: &str) -> bool {
        self.verifying.lock().unwrap().contains(root_id)
    }
}

// ---------------------------------------------------------------------------
// Reading files back

/// Reads one file and returns its hash, or `None` if its size or date
/// changed since it was indexed (then it was edited, not damaged).
fn rehash(path: &std::path::Path, size: i64, modified_at: Option<i64>) -> std::io::Result<Option<String>> {
    let meta = std::fs::metadata(path)?;
    if meta.len() as i64 != size || meta.modified().ok().and_then(system_time_ms) != modified_at {
        return Ok(None);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(std::fs::File::open(path)?)?;
    Ok(Some(hasher.finalize().to_hex().to_string()))
}

/// Reads every hashed file of a storage location and compares it with its
/// hash. Returns `None` if it was cancelled.
async fn verify_root(s: &SharedState, root: &StorageRoot, by: &str) -> Result<Option<RootCheck>> {
    s.storage.ensure_online(root)?;
    let job = s.jobs.add("check", format!("Verify {}", root.name), "This computer".into(), by.to_string());
    let Some(_slot) = s.jobs.start(s, &job).await else { return Ok(None) };
    let stats = s.db.root_stats(&root.id).await.unwrap_or_default();
    job.update(|i| {
        i.files_total = stats.files.max(0) as u64;
        i.bytes_total = stats.bytes.max(0) as u64;
    });
    let base = PathBuf::from(&root.absolute_path);
    let mut after = String::new();
    let (mut files, mut bytes) = (0i64, 0i64);
    let mut damaged: Vec<DamagedFile> = Vec::new();
    let mut cancelled = false;
    'outer: loop {
        let batch = s.db.hashed_files(&root.id, &after, BATCH).await?;
        let Some(last) = batch.last() else { break };
        after = last.relative_path.clone();
        for f in batch {
            if !s.jobs.checkpoint(s, &job).await {
                cancelled = true;
                break 'outer;
            }
            if !StorageManager::is_online(root) {
                finish_job(s, &job);
                return Err(NexusError::Unavailable(format!("{} disappeared while it was being verified", root.name)));
            }
            job.update(|i| i.current = Some(f.relative_path.clone()));
            let path = disk_path(&base, &f.relative_path);
            let (size, modified) = (f.size, f.modified_at);
            let outcome = tokio::task::spawn_blocking(move || rehash(&path, size, modified)).await.map_err(NexusError::internal)?;
            let found = match outcome {
                Ok(Some(hash)) if hash == f.content_hash => None,
                Ok(Some(hash)) => Some(Some(hash)),
                Ok(None) => None, // changed since indexing: the indexer will catch up
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => Some(None), // could not be read
            };
            if let Some(found_hash) = found {
                damaged.push(DamagedFile {
                    file_id: f.id.clone(),
                    root_id: root.id.clone(),
                    relative_path: f.relative_path.clone(),
                    size: f.size,
                    expected_hash: f.content_hash.clone(),
                    found_hash,
                    found_at: now_ms(),
                });
            }
            files += 1;
            bytes += f.size.max(0);
            job.update(|i| i.files_done += 1);
            job.set_bytes(bytes as u64);
        }
    }
    finish_job(s, &job);
    if cancelled {
        return Ok(None);
    }
    let result = match damaged.len() {
        0 => format!("{} file{} read, all intact", files, if files == 1 { "" } else { "s" }),
        n => format!("{} file{} read, {n} damaged", files, if files == 1 { "" } else { "s" }),
    };
    let check = RootCheck { root_id: root.id.clone(), checked_at: now_ms(), files, bytes, damaged: damaged.len() as i64, result };
    s.db.record_root_check(&check, &damaged).await?;
    if !damaged.is_empty() {
        let _ = s
            .db
            .log_activity("storage.damaged", Some(&format!("{}: {} damaged files", root.name, damaged.len())), None, Some(&s.node.device_id))
            .await;
    }
    Ok(Some(check))
}

/// The file's place on disk ('/'-separated relative path under the root).
fn disk_path(base: &std::path::Path, rel: &str) -> PathBuf {
    rel.split('/').filter(|p| !p.is_empty()).fold(base.to_path_buf(), |p, part| p.join(part))
}

async fn verify_and_record(s: &SharedState, root: &StorageRoot, by: &str) {
    s.health.verifying.lock().unwrap().insert(root.id.clone());
    s.events.emit(Event::HealthUpdated);
    if let Err(e) = verify_root(s, root, by).await {
        tracing::warn!(root = %root.name, error = %e, "could not verify a storage location");
    }
    s.health.verifying.lock().unwrap().remove(&root.id);
    s.events.emit(Event::HealthUpdated);
}

// ---------------------------------------------------------------------------
// Watching

async fn read_disks(s: &AppState) {
    let result = tokio::task::spawn_blocking(health::physical_disks).await.unwrap_or_else(|e| Err(e.to_string()));
    if let Err(e) = &result {
        tracing::warn!(error = %e, "could not read disk health");
    }
    *s.health.disks.lock().unwrap() = Some((now_ms(), result));
    s.events.emit(Event::HealthUpdated);
}

/// Notes storage locations that went missing or came back.
async fn watch_presence(s: &AppState) {
    let Ok(roots) = s.db.list_roots().await else { return };
    let known: HashMap<String, bool> = s.db.root_presence().await.unwrap_or_default().into_iter().map(|(id, on, _)| (id, on)).collect();
    let mut changed = false;
    for root in roots.into_iter().filter(|r| r.enabled) {
        let path = root.clone();
        let online = tokio::task::spawn_blocking(move || StorageManager::is_online(&path)).await.unwrap_or(false);
        let before = known.get(&root.id).copied();
        if before == Some(online) {
            continue;
        }
        if s.db.set_root_presence(&root.id, online).await.is_err() {
            continue;
        }
        changed = true;
        if before.is_some() {
            let action = if online { "storage.reconnected" } else { "storage.disconnected" };
            let _ = s.db.log_activity(action, Some(&root.name), None, Some(&s.node.device_id)).await;
            tracing::info!(root = %root.name, online, "storage location presence changed");
        }
    }
    if changed {
        s.events.emit(Event::HealthUpdated);
    }
}

pub fn spawn(s: SharedState) {
    let started = now_ms();
    tokio::spawn(async move {
        loop {
            watch_presence(&s).await;

            let refresh = std::mem::take(&mut *s.health.refresh_disks.lock().unwrap());
            let stale = s.health.disks.lock().unwrap().as_ref().is_none_or(|(at, _)| now_ms() - at >= DISKS_EVERY_MS);
            if refresh || stale {
                read_disks(&s).await;
            }

            // One full read at a time, in the background.
            if s.health.verifying.lock().unwrap().is_empty() {
                let asked: HashMap<String, String> = s.health.verify_now.lock().unwrap().drain().collect();
                let checks: HashMap<String, i64> =
                    s.db.root_checks().await.unwrap_or_default().into_iter().map(|c| (c.root_id, c.checked_at)).collect();
                let roots = s.db.list_roots().await.unwrap_or_default();
                let due = roots.into_iter().find(|r| {
                    let wanted = asked.contains_key(&r.id)
                        || (now_ms() - started >= VERIFY_DELAY_MS
                            && checks.get(&r.id).is_none_or(|at| now_ms() - at >= VERIFY_EVERY_MS));
                    wanted && StorageManager::is_online(r) && !s.indexer.is_scanning(&r.id)
                });
                // Asked for but not started now: keep them for the next round.
                if let Some(root) = due {
                    let by = asked.get(&root.id).cloned().unwrap_or_else(|| "NEXUS".into());
                    s.health.verify_now.lock().unwrap().extend(asked.into_iter().filter(|(id, _)| *id != root.id));
                    s.health.verifying.lock().unwrap().insert(root.id.clone());
                    let s = s.clone();
                    tokio::spawn(async move {
                        verify_and_record(&s, &root, &by).await;
                        s.health.wake.notify_one();
                    });
                } else {
                    s.health.verify_now.lock().unwrap().extend(asked);
                }
            }

            tokio::select! {
                _ = s.health.wake.notified() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Repair

/// Where a damaged file's right content can come from, best first.
enum Source {
    /// The same file in a copy or sync on a paired device.
    Pair(SyncPair),
    /// A file in a backup snapshot.
    Backup(nexus_database::SnapshotSet, nexus_database::SnapshotFile),
}

/// `rel` inside `base`, if it is there ('' is everything).
fn inside<'a>(base: &str, rel: &'a str) -> Option<&'a str> {
    if base.is_empty() {
        return Some(rel);
    }
    let rest = rel.get(base.len()..)?;
    (rel[..base.len()].eq_ignore_ascii_case(base) && rest.starts_with('/')).then(|| &rest[1..])
}

async fn sources(s: &AppState, d: &DamagedFile) -> Vec<Source> {
    let mut out = Vec::new();
    for pair in s.db.list_sync_pairs().await.unwrap_or_default() {
        if pair.local_root_id == d.root_id && pair.mode != "download" && inside(&pair.local_path, &d.relative_path).is_some() {
            out.push(Source::Pair(pair));
        }
    }
    for set in s.db.list_snapshot_sets().await.unwrap_or_default() {
        if set.local_root_id != d.root_id {
            continue;
        }
        let Some(sub) = inside(&set.local_path, &d.relative_path) else { continue };
        for snap in s.db.list_snapshots(&set.id).await.unwrap_or_default() {
            let Ok((_, files)) = s.db.get_snapshot(&set.id, &snap.id).await else { continue };
            if let Some(f) = files.into_iter().find(|f| f.p.eq_ignore_ascii_case(sub) && f.h == d.expected_hash) {
                out.push(Source::Backup(set.clone(), f));
                break;
            }
        }
    }
    out
}

fn source_name(s: &Source, devices: &HashMap<String, String>) -> String {
    match s {
        Source::Pair(p) => format!(
            "the {} on {}",
            if p.mode == "copy" { "copy" } else { "synced folder" },
            devices.get(&p.device_id).map(String::as_str).unwrap_or("a paired device")
        ),
        Source::Backup(set, _) => format!("the backup {}", set.name),
    }
}

/// Puts the right content back from the first source that has it.
async fn repair(s: &SharedState, user: &AuthUser, d: &DamagedFile, job: &std::sync::Arc<super::transfers::Job>) -> Result<String> {
    let devices: HashMap<String, String> = s.db.list_devices().await?.into_iter().map(|d| (d.id, d.name)).collect();
    let (parent, name) = paths::split_parent(&d.relative_path);
    let mover = Mover { s, user };
    let mut tried: Vec<String> = Vec::new();
    for source in sources(s, d).await {
        let label = source_name(&source, &devices);
        let outcome: Result<()> = async {
            match &source {
                Source::Pair(pair) => {
                    let sub = inside(&pair.local_path, &d.relative_path).unwrap_or_default();
                    let device = peer_target(s, &pair.device_id).await?;
                    let side = Side::Peer(device);
                    let (sub_parent, sub_name) = paths::split_parent(sub);
                    let folder = paths::join(&pair.remote_path, &sub_parent);
                    let item = mover
                        .tree(&side, &pair.remote_root_id, &folder)
                        .await?
                        .into_iter()
                        .find(|i| !i.is_dir && i.path.eq_ignore_ascii_case(&sub_name))
                        .ok_or_else(|| NexusError::NotFound("The file there".into()))?;
                    if item.content_hash.as_deref() != Some(d.expected_hash.as_str()) {
                        return Err(NexusError::Conflict("It holds different content".into()));
                    }
                    let meta = Meta {
                        id: item.id.clone(),
                        root_id: pair.remote_root_id.clone(),
                        relative_path: paths::join(&folder, &item.path),
                        filename: sub_name.clone(),
                        is_dir: false,
                        size: item.size,
                        modified_at: item.modified_at,
                        // The copy arrives only if it has exactly this content.
                        content_hash: Some(d.expected_hash.clone()),
                    };
                    let req = NewUpload {
                        root_id: d.root_id.clone(),
                        path: parent.clone(),
                        name: name.clone(),
                        size: item.size as u64,
                        on_conflict: Some("replace".into()),
                        modified_at: item.modified_at,
                        make_parents: true,
                    };
                    mover.send_file(job, &side, &meta, &Side::Local, req).await.map(|_| ())
                }
                Source::Backup(set, file) => super::snapshots::repair_file(s, user, job, set, file, &parent).await,
            }
        }
        .await;
        match outcome {
            Ok(()) => return Ok(label),
            Err(e) => tried.push(format!("{label}: {e}")),
        }
    }
    Err(NexusError::NotFound(if tried.is_empty() {
        "A copy or backup with this file's right content".into()
    } else {
        format!("A good copy ({})", tried.join("; "))
    }))
}

// ---------------------------------------------------------------------------
// Views

#[derive(Serialize)]
pub struct DiskView {
    #[serde(flatten)]
    disk: PhysicalDisk,
    /// NEXUS storage locations on this disk.
    storage: Vec<String>,
}

#[derive(Serialize)]
pub struct DamagedView {
    file_id: String,
    path: String,
    size: i64,
    found_at: i64,
    /// Could not be read at all (rather than read with the wrong content).
    unreadable: bool,
    /// Where a repair would take the right content from.
    repair_from: Vec<String>,
}

#[derive(Serialize)]
pub struct RootHealth {
    id: String,
    name: String,
    path: String,
    online: bool,
    /// When it went missing or came back.
    since: Option<i64>,
    removable: bool,
    total_bytes: Option<u64>,
    available_bytes: Option<u64>,
    /// The physical disk it is on.
    disk: Option<String>,
    last_check: Option<RootCheck>,
    verifying: bool,
    damaged: Vec<DamagedView>,
}

#[derive(Serialize)]
pub struct Problem {
    /// `error` (act now) or `warning`.
    level: &'static str,
    text: String,
    root_id: Option<String>,
}

#[derive(Serialize)]
pub struct HealthView {
    checked_at: Option<i64>,
    disks: Vec<DiskView>,
    disks_error: Option<String>,
    roots: Vec<RootHealth>,
    problems: Vec<Problem>,
}

fn letter_of(path: &str) -> Option<String> {
    let mut chars = path.chars();
    let (l, colon) = (chars.next()?, chars.next()?);
    (l.is_ascii_alphabetic() && colon == ':').then(|| l.to_ascii_uppercase().to_string())
}

fn drive_of<'a>(path: &str, drives: &'a [DriveInfo]) -> Option<&'a DriveInfo> {
    let path = path.to_lowercase();
    drives.iter().filter(|d| path.starts_with(&d.mount_point.to_lowercase())).max_by_key(|d| d.mount_point.len())
}

fn human(b: u64) -> String {
    super::snapshots::human_bytes(b as i64)
}

/// Free space that calls for attention: `Some((level, words))`.
pub fn space_problem(total: u64, free: u64) -> Option<(&'static str, &'static str)> {
    if total == 0 {
        return None;
    }
    let pct = free as f64 * 100.0 / total as f64;
    if pct < 3.0 || free < 1 << 30 {
        Some(("error", "almost full"))
    } else if pct < 10.0 {
        Some(("warning", "nearly full"))
    } else {
        None
    }
}

/// Problems a disk report shows.
pub fn disk_problems(d: &PhysicalDisk) -> Vec<(&'static str, String)> {
    let name = if d.letters.is_empty() { d.model.clone() } else { format!("{} ({}:)", d.model, d.letters.join(":, ")) };
    let mut out = Vec::new();
    match d.health.as_str() {
        "unhealthy" => out.push(("error", format!("Windows reports {name} as failing. Copy what is on it elsewhere now."))),
        "warning" => out.push(("warning", format!("Windows reports a problem with {name}{}.", if d.status.is_empty() || d.status == "OK" { String::new() } else { format!(": {}", d.status) }))),
        _ => {}
    }
    let hot = if d.media == "ssd" { 70 } else { 55 };
    if let Some(t) = d.temperature_c.filter(|t| *t >= hot) {
        out.push(("warning", format!("{name} is running hot ({t} °C).")));
    }
    if let Some(w) = d.wear_percent.filter(|w| *w >= 90) {
        out.push(("warning", format!("{name} has used {w}% of its rated life.")));
    }
    let errors = d.read_errors.unwrap_or(0) + d.write_errors.unwrap_or(0);
    if errors > 0 {
        out.push(("warning", format!("{name} reported {errors} errors it could not correct.")));
    }
    out
}

async fn build(s: &AppState, user: &AuthUser) -> Result<HealthView> {
    let drives = tokio::task::spawn_blocking(drives::list_drives).await.unwrap_or_default();
    let (checked_at, disks, disks_error) = match s.health.disks.lock().unwrap().clone() {
        Some((at, Ok(disks))) => (Some(at), disks, None),
        Some((at, Err(e))) => (Some(at), Vec::new(), Some(e)),
        None => (None, Vec::new(), None),
    };
    let presence: HashMap<String, (bool, i64)> = s.db.root_presence().await?.into_iter().map(|(id, on, at)| (id, (on, at))).collect();
    let checks: HashMap<String, RootCheck> = s.db.root_checks().await?.into_iter().map(|c| (c.root_id.clone(), c)).collect();
    let mut damaged: HashMap<String, Vec<DamagedFile>> = HashMap::new();
    for d in s.db.damaged_files().await? {
        damaged.entry(d.root_id.clone()).or_default().push(d);
    }
    let devices: HashMap<String, String> = s.db.list_devices().await?.into_iter().map(|d| (d.id, d.name)).collect();
    let mut problems: Vec<Problem> = Vec::new();
    let mut roots = Vec::new();
    let mut on_disk: HashMap<String, Vec<String>> = HashMap::new();

    for root in s.db.list_roots().await?.into_iter().filter(|r| user.sees_root(&r.id)) {
        let online = StorageManager::is_online(&root);
        let drive = online.then(|| drive_of(&root.absolute_path, &drives)).flatten();
        let letter = drive.and_then(|d| letter_of(&d.mount_point)).or_else(|| letter_of(&root.absolute_path));
        let disk = letter.as_ref().and_then(|l| disks.iter().find(|d| d.letters.contains(l)));
        if let Some(disk) = disk {
            on_disk.entry(disk.number.clone()).or_default().push(root.name.clone());
        }
        let since = presence.get(&root.id).map(|(_, at)| *at);
        if root.enabled && !online {
            problems.push(Problem {
                level: "error",
                text: format!("{} is missing: its drive or folder cannot be reached ({}).", root.name, root.absolute_path),
                root_id: Some(root.id.clone()),
            });
        }
        if let Some(d) = drive
            && let Some((level, words)) = space_problem(d.total_bytes, d.available_bytes)
        {
            problems.push(Problem {
                level,
                text: format!("The drive holding {} is {words}: {} free of {}.", root.name, human(d.available_bytes), human(d.total_bytes)),
                root_id: Some(root.id.clone()),
            });
        }
        let mut views = Vec::new();
        for d in damaged.remove(&root.id).unwrap_or_default() {
            let from = sources(s, &d).await.iter().map(|src| source_name(src, &devices)).collect();
            views.push(DamagedView {
                file_id: d.file_id,
                path: d.relative_path,
                size: d.size,
                found_at: d.found_at,
                unreadable: d.found_hash.is_none(),
                repair_from: from,
            });
        }
        if !views.is_empty() {
            problems.push(Problem {
                level: "error",
                text: format!(
                    "{} file{} in {} {} damaged on the disk.",
                    views.len(),
                    if views.len() == 1 { "" } else { "s" },
                    root.name,
                    if views.len() == 1 { "is" } else { "are" }
                ),
                root_id: Some(root.id.clone()),
            });
        }
        roots.push(RootHealth {
            online,
            since,
            removable: drive.is_some_and(|d| d.removable),
            total_bytes: drive.map(|d| d.total_bytes),
            available_bytes: drive.map(|d| d.available_bytes),
            disk: disk.map(|d| d.number.clone()),
            last_check: checks.get(&root.id).cloned(),
            verifying: s.health.is_verifying(&root.id),
            damaged: views,
            id: root.id,
            name: root.name,
            path: root.absolute_path,
        });
    }

    for d in &disks {
        for (level, text) in disk_problems(d) {
            problems.push(Problem { level, text, root_id: None });
        }
    }
    // Copies that have not been brought up to date for a day.
    for pair in s.db.list_sync_pairs().await?.into_iter().filter(|p| p.mode == "copy" && p.enabled && user.sees_root(&p.local_root_id)) {
        let behind = pair.last_error.is_some() && now_ms() - pair.last_ok_at.unwrap_or(pair.created_at) > DAY_MS;
        if behind {
            let device = devices.get(&pair.device_id).cloned().unwrap_or_else(|| "a paired device".into());
            problems.push(Problem {
                level: "warning",
                text: format!("The copy on {device} ({}) has not been brought up to date for over a day.", pair.remote_label),
                root_id: Some(pair.local_root_id.clone()),
            });
        }
    }
    problems.sort_by_key(|p| p.level != "error");

    let disks = disks
        .into_iter()
        .map(|d| DiskView { storage: on_disk.remove(&d.number).unwrap_or_default(), disk: d })
        .collect();
    Ok(HealthView { checked_at, disks, disks_error, roots, problems })
}

// ---------------------------------------------------------------------------
// Routes

/// `GET /storage/health`
pub async fn get(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<HealthView>> {
    user.require(Action::Read)?;
    Ok(Json(build(&s, &user).await?))
}

/// `POST /storage/health/refresh`: reads the disks again now.
pub async fn refresh(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<HealthView>> {
    user.require(Action::Read)?;
    read_disks(&s).await;
    watch_presence(&s).await;
    Ok(Json(build(&s, &user).await?))
}

/// `POST /storage/roots/{id}/verify`: reads the storage location in full soon.
pub async fn verify(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    user.require_root(&id)?;
    let root = s.db.get_root(&id).await?;
    s.storage.ensure_online(&root)?;
    if s.health.is_verifying(&id) {
        return Err(NexusError::Conflict(format!("{} is being verified already", root.name)).into());
    }
    s.health.verify_now.lock().unwrap().insert(id, user.username.clone());
    s.health.wake.notify_one();
    audit(&s, &user, "storage.verify", Some(&root.name)).await;
    Ok(StatusCode::ACCEPTED)
}

async fn damaged_for(s: &AppState, user: &AuthUser, file_id: &str) -> Result<DamagedFile> {
    let d = s.db.damaged_file(file_id).await?.ok_or_else(|| NexusError::NotFound("Damaged file".into()))?;
    user.require_root(&d.root_id).map_err(|e| e.0)?;
    Ok(d)
}

#[derive(Serialize)]
pub struct Started {
    job_id: String,
}

/// `POST /storage/health/damaged/{file_id}/repair`
pub async fn repair_damaged(State(s): State<SharedState>, user: AuthUser, Path(file_id): Path<String>) -> ApiResult<(StatusCode, Json<Started>)> {
    user.require_person()?;
    user.require(Action::Write)?;
    let d = damaged_for(&s, &user, &file_id).await?;
    if sources(&s, &d).await.is_empty() {
        return Err(NexusError::invalid("No extra copy or backup holds this file's right content").into());
    }
    let job = s.jobs.add("check", format!("Repair {}", paths::split_parent(&d.relative_path).1), "→ this computer".into(), user.username.clone());
    job.update(|i| {
        i.files_total = 1;
        i.bytes_total = d.size.max(0) as u64;
    });
    let job_id = job.id.clone();
    tokio::spawn(async move {
        let Some(_slot) = s.jobs.start(&s, &job).await else { return };
        match repair(&s, &user, &d, &job).await {
            Ok(from) => {
                job.update(|i| i.files_done = 1);
                let _ = s.db.clear_damaged(&d.file_id).await;
                audit(&s, &user, "storage.repaired", Some(&format!("{} from {from}", d.relative_path))).await;
            }
            Err(e) => job.fail_file(&d.relative_path, &e),
        }
        finish_job(&s, &job);
        s.events.emit(Event::HealthUpdated);
    });
    Ok((StatusCode::ACCEPTED, Json(Started { job_id })))
}

/// `POST /storage/health/damaged/{file_id}/accept`: the file is fine as it
/// is now (it was changed on purpose); remember its current content.
pub async fn accept_damaged(State(s): State<SharedState>, user: AuthUser, Path(file_id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::Write)?;
    let d = damaged_for(&s, &user, &file_id).await?;
    if let Some(found) = &d.found_hash
        && let Ok(file) = s.db.get_file(&d.file_id).await
    {
        s.db.set_hash(&file.id, Some(found), file.modified_at, now_ms()).await?;
    }
    s.db.clear_damaged(&d.file_id).await?;
    audit(&s, &user, "storage.damage_accepted", Some(&d.relative_path)).await;
    s.events.emit(Event::HealthUpdated);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_levels() {
        let tb = 1u64 << 40;
        assert_eq!(space_problem(tb, tb / 2), None);
        assert_eq!(space_problem(tb, tb / 20).map(|p| p.0), Some("warning"));
        assert_eq!(space_problem(tb, tb / 100).map(|p| p.0), Some("error"));
        // Small drives: under 1 GB free is almost full whatever the share.
        assert_eq!(space_problem(4 << 30, 900 << 20).map(|p| p.0), Some("error"));
        assert_eq!(space_problem(0, 0), None);
    }

    #[test]
    fn disk_reports_become_problems() {
        let mut d = PhysicalDisk {
            number: "0".into(),
            model: "Disk".into(),
            media: "hdd".into(),
            bus: "SATA".into(),
            health: "healthy".into(),
            status: "OK".into(),
            size: 1,
            letters: vec!["D".into()],
            temperature_c: Some(40),
            temperature_max_c: None,
            wear_percent: None,
            power_on_hours: None,
            read_errors: Some(0),
            write_errors: Some(0),
            details: true,
        };
        assert!(disk_problems(&d).is_empty());
        d.health = "unhealthy".into();
        d.temperature_c = Some(60);
        d.read_errors = Some(3);
        let p = disk_problems(&d);
        assert_eq!(p.iter().map(|p| p.0).collect::<Vec<_>>(), ["error", "warning", "warning"]);
        assert!(p[0].1.contains("Disk (D:)"));
    }

    #[test]
    fn paths_inside_folders() {
        assert_eq!(inside("", "a/b.txt"), Some("a/b.txt"));
        assert_eq!(inside("Docs", "docs/a/b.txt"), Some("a/b.txt"));
        assert_eq!(inside("Docs", "Docs2/b.txt"), None);
        assert_eq!(inside("Docs", "Docs"), None);
        assert_eq!(letter_of("d:\\Media"), Some("D".into()));
        assert_eq!(letter_of("\\\\nas\\share"), None);
    }

    #[test]
    fn damage_is_told_apart_from_editing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"hello").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let mtime = meta.modified().ok().and_then(system_time_ms);
        let good = blake3::hash(b"hello").to_hex().to_string();
        assert_eq!(rehash(&path, 5, mtime).unwrap(), Some(good));
        // Different size or date: edited, not damaged.
        assert_eq!(rehash(&path, 6, mtime).unwrap(), None);
        assert_eq!(rehash(&path, 5, mtime.map(|m| m - 1000)).unwrap(), None);
        assert!(rehash(&dir.path().join("gone"), 5, mtime).is_err());
    }
}
