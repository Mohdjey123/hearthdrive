//! The phone side: backing up photos and videos to a computer, and sending
//! files shared to the app from other apps.
//!
//! **Backup.** Photos and videos in the phone's shared storage (`DCIM`,
//! `Pictures`, `Movies`) are copied to a folder on a paired computer, keeping
//! their folders and dates. It runs on the phone's node while the app is open:
//! soon after it starts, every 15 minutes, and when asked. Each run sends only
//! what is new or changed. Deleting a photo on the phone leaves the copy on
//! the computer, and a copy deleted on the computer is not sent again. A file
//! with the same name and size already on the computer (after reinstalling
//! the app, say) is not sent twice.
//!
//! **Share menu.** The Android app copies files shared to it into an inbox
//! folder; the person picks a computer and folder, and they are sent from
//! there like any copy and then removed from the inbox.

use super::audit;
use super::files::percent_encode;
use super::transfers::{Mover, Side, finish_job, peer_target};
use super::uploads::NewUpload;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::peers::{Acting, CALL_TIMEOUT};
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::State;
use axum::http::{Method, StatusCode};
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::{NexusError, Result};
use nexus_database::{Backup, NewBackup};
use nexus_security::Action;
use nexus_storage::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

/// Phone folders that can be backed up, and how they are shown.
pub const SOURCES: &[(&str, &str)] = &[("DCIM", "Camera and screenshots"), ("Pictures", "Pictures"), ("Movies", "Videos")];

/// A backup runs at least this often while the app is open.
const INTERVAL_MS: i64 = 15 * 60 * 1000;
/// After a failed run (the computer was off, say), try again this soon.
const RETRY_MS: i64 = 2 * 60 * 1000;
/// Files changed more recently than this may still be being written.
const SETTLE_MS: i64 = 30 * 1000;
const TICK: Duration = Duration::from_secs(30);
/// Leave the network a moment to connect after the app starts.
const FIRST_CHECK: Duration = Duration::from_secs(15);
/// Folders deeper than this are not looked into.
const MAX_DEPTH: usize = 24;

#[derive(Default)]
pub struct BackupControl {
    wake: Notify,
    forced: AtomicBool,
    running: AtomicBool,
}

impl BackupControl {
    pub fn run_now(&self) {
        self.forced.store(true, Ordering::Relaxed);
        self.wake.notify_one();
    }
}

/// A photo or video found on the phone.
#[derive(Debug, Clone, PartialEq)]
struct Found {
    /// Path under the shared storage, such as `DCIM/Camera/IMG_1.jpg`.
    rel: String,
    path: PathBuf,
    size: u64,
    modified: i64,
}

impl Found {
    fn fingerprint(&self) -> String {
        format!("{}:{}", self.size, self.modified)
    }
}

fn modified_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Lists the files under the chosen phone folders. Hidden files and
/// folders (thumbnails, files still being written or in the phone's trash,
/// which Android names with a leading dot) are skipped, as are files changed
/// in the last half minute.
fn scan(media: &Path, sources: &[String], now: i64) -> std::io::Result<Vec<Found>> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, String, usize)> = Vec::new();
    for source in sources {
        let dir = media.join(source);
        if dir.is_dir() {
            stack.push((dir, source.clone(), 0));
        }
    }
    while let Some((dir, rel, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && depth == 0 => return Err(e),
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || paths::validate_name(&name).is_err() {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let child_rel = format!("{rel}/{name}");
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((entry.path(), child_rel, depth + 1));
                }
            } else if kind.is_file() {
                let Ok(meta) = entry.metadata() else { continue };
                let modified = modified_ms(&meta);
                if now - modified < SETTLE_MS || meta.len() == 0 {
                    continue;
                }
                out.push(Found { rel: child_rel, path: entry.path(), size: meta.len(), modified });
            }
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// The person a backup acts for, as they are now.
async fn acting_user(s: &AppState, username: &str) -> Result<AuthUser> {
    let (user, _) = s
        .db
        .find_credentials(username)
        .await?
        .ok_or_else(|| NexusError::PermissionDenied(format!("{username} no longer has an account here")))?;
    Ok(AuthUser::person(user))
}

/// Makes "not allowed" from the computer say what to do about it.
fn explain(e: NexusError, device: &str) -> NexusError {
    match e {
        NexusError::PermissionDenied(_) => NexusError::PermissionDenied(format!(
            "{device} only lets this phone look at files. On {device}, open Devices and give this phone the Editor role."
        )),
        NexusError::NotFound(_) => NexusError::NotFound(format!("The backup folder on {device}")),
        other => other,
    }
}

/// Runs a backup now. Returns a summary for the status line.
async fn run_backup(s: &AppState, backup: &Backup) -> Result<String> {
    let media = s.media_dir.clone().ok_or_else(|| NexusError::Unavailable("This device has no photos to back up".into()))?;
    let user = acting_user(s, &backup.created_by).await?;
    user.require(Action::Write).map_err(|e| e.0)?;
    let target = peer_target(s, &backup.device_id).await?;
    let device = target.name.clone();
    let side = Side::Peer(target);
    let mover = Mover { s, user: &user };

    let sources: Vec<String> =
        backup.source_list().into_iter().filter(|src| SOURCES.iter().any(|(k, _)| k == src)).collect();
    let now = now_ms();
    let found = tokio::task::spawn_blocking(move || scan(&media, &sources, now))
        .await
        .map_err(NexusError::internal)?
        .map_err(|_| NexusError::PermissionDenied("NEXUS is not allowed to read your photos and videos".into()))?;
    let done = s.db.backed_up(&backup.id).await?;
    let todo: Vec<&Found> = found.iter().filter(|f| done.get(&f.rel) != Some(&f.fingerprint())).collect();
    if todo.is_empty() {
        return Ok(format!("Up to date: {} photos and videos", found.len()));
    }

    // What the computer already has, to skip copies made before (after the
    // app was reinstalled, say). A missing folder is created on the way.
    let remote: HashMap<String, i64> = match mover.tree(&side, &backup.root_id, &backup.path).await {
        Ok(items) => items.into_iter().filter(|i| !i.is_dir).map(|i| (i.path.to_lowercase(), i.size)).collect(),
        Err(NexusError::NotFound(_)) => HashMap::new(),
        Err(e) => return Err(explain(e, &device)),
    };

    let job = s.jobs.add("backup", "Back up photos and videos".into(), format!("This phone → {device}"), user.username.clone());
    let Some(_slot) = s.jobs.start(s, &job).await else { return Ok("Cancelled".into()) };
    job.update(|i| {
        i.files_total = todo.len() as u64;
        i.bytes_total = todo.iter().map(|f| f.size).sum();
    });

    let mut sent = 0u64;
    let mut failure = None;
    for f in &todo {
        if job.is_cancelled() {
            break;
        }
        let (parent, name) = paths::split_parent(&f.rel);
        let known = done.contains_key(&f.rel);
        if !known && remote.get(&f.rel.to_lowercase()) == Some(&(f.size as i64)) {
            s.db.mark_backed_up(&backup.id, &f.rel, &f.fingerprint()).await?;
            job.update(|i| {
                i.files_done += 1;
                i.bytes_total -= f.size;
            });
            continue;
        }
        job.update(|i| i.current = Some(name.clone()));
        let req = NewUpload {
            root_id: backup.root_id.clone(),
            path: paths::join(&backup.path, &parent),
            name: name.clone(),
            size: f.size,
            // A photo changed on the phone replaces its copy (the computer
            // keeps the old one as a version); anything else never
            // overwrites a different file of the same name.
            on_conflict: Some(if known { "replace" } else { "rename" }.into()),
            modified_at: Some(f.modified),
            make_parents: true,
        };
        match mover.send_disk_file(&job, &f.path, &side, req).await {
            Ok(_) => {
                s.db.mark_backed_up(&backup.id, &f.rel, &f.fingerprint()).await?;
                sent += 1;
                job.update(|i| i.files_done += 1);
            }
            // The file went away or changed while it was read: next run.
            Err(NexusError::Io(_)) | Err(NexusError::InvalidInput(_)) if std::fs::metadata(&f.path).map(|m| m.len()).ok() != Some(f.size) => {
                job.update(|i| i.bytes_total -= f.size);
            }
            Err(e) => {
                job.fail_file(&name, &e);
                // Losing the computer or the permission ends the run.
                if matches!(e, NexusError::PeerUnreachable(_) | NexusError::PermissionDenied(_) | NexusError::InsufficientStorage(_)) {
                    failure = Some(explain(e, &device));
                    break;
                }
            }
        }
    }
    finish_job(s, &job);
    if job.is_cancelled() {
        return Ok(format!("Stopped after {sent} files"));
    }
    if let Some(e) = failure {
        return Err(e);
    }
    let errors = job.error_count();
    if sent > 0 {
        audit(s, &user, "backup.ran", Some(&format!("{sent} file(s) to {device}"))).await;
    }
    Ok(match (sent, errors) {
        (0, 0) => format!("Up to date: {} photos and videos", found.len()),
        (n, 0) => format!("Backed up {n} new file{}", if n == 1 { "" } else { "s" }),
        (n, e) => format!("Backed up {n} file{}; {e} could not be sent", if n == 1 { "" } else { "s" }),
    })
}

async fn run_and_record(s: &AppState) {
    let Ok(Some(backup)) = s.db.get_backup().await else { return };
    if s.backup.running.swap(true, Ordering::SeqCst) {
        return;
    }
    s.events.emit(Event::SyncUpdated { id: backup.id.clone() });
    let result = run_backup(s, &backup).await;
    let (ok, text, error) = match result {
        Ok(text) => (true, text, None),
        Err(e @ NexusError::PeerUnreachable(_)) => (false, "Waiting for the computer".to_string(), Some(e.to_string())),
        Err(e) => (false, "Needs attention".to_string(), Some(e.to_string())),
    };
    if let Err(e) = s.db.record_backup_run(&backup.id, ok, &text, error.as_deref()).await {
        tracing::warn!(error = %e, "could not record a backup run");
    }
    s.backup.running.store(false, Ordering::SeqCst);
    s.events.emit(Event::SyncUpdated { id: backup.id });
}

/// Runs the backup when it is due or asked for (phones only).
pub fn spawn(s: SharedState) {
    if s.media_dir.is_none() {
        return;
    }
    tokio::spawn(async move {
        tokio::select! {
            _ = s.backup.wake.notified() => {}
            _ = tokio::time::sleep(FIRST_CHECK) => {}
        }
        loop {
            let forced = s.backup.forced.swap(false, Ordering::Relaxed);
            if let Ok(Some(b)) = s.db.get_backup().await {
                let last = b.last_run_at.unwrap_or(0);
                let failed = b.last_error.is_some();
                let now = now_ms();
                let due = now - last >= INTERVAL_MS || (failed && now - last >= RETRY_MS);
                if forced || (b.enabled && due) {
                    run_and_record(&s).await;
                }
            }
            tokio::select! {
                _ = s.backup.wake.notified() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Backup routes

#[derive(Serialize)]
pub struct SourceView {
    key: &'static str,
    label: &'static str,
}

#[derive(Serialize)]
pub struct BackupView {
    #[serde(flatten)]
    backup: Backup,
    sources: Vec<String>,
    device_name: String,
}

#[derive(Serialize)]
pub struct BackupStatus {
    /// Whether this device has photos to back up (phones).
    available: bool,
    choices: Vec<SourceView>,
    backup: Option<BackupView>,
    running: bool,
}

async fn status_of(s: &AppState) -> Result<BackupStatus> {
    let backup = match s.db.get_backup().await? {
        Some(b) => {
            let device_name = s.db.get_device(&b.device_id).await.map(|d| d.name).unwrap_or_else(|_| "Removed device".into());
            let sources = b.source_list();
            Some(BackupView { backup: b, sources, device_name })
        }
        None => None,
    };
    Ok(BackupStatus {
        available: s.media_dir.is_some(),
        choices: SOURCES.iter().map(|(key, label)| SourceView { key, label }).collect(),
        backup,
        running: s.backup.running.load(Ordering::Relaxed),
    })
}

/// Only a person on this phone manages its backup.
fn require_phone_person(user: &AuthUser) -> Result<(), crate::error::ApiError> {
    user.require_person()?;
    user.require(Action::Write)?;
    if user.user_id.is_none() {
        return Err(NexusError::PermissionDenied("Sign in with an account to set up backup".into()).into());
    }
    Ok(())
}

pub async fn get(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<BackupStatus>> {
    user.require_person()?;
    Ok(Json(status_of(&s).await?))
}

#[derive(Deserialize)]
pub struct SetBackup {
    device_id: String,
    root_id: String,
    #[serde(default)]
    path: String,
    sources: Vec<String>,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

/// Checks that a folder on a paired device exists and can be written to,
/// as `user`. Returns "Storage / folder" for display.
async fn check_destination(s: &AppState, user: &AuthUser, device_id: &str, root_id: &str, path: &str) -> Result<String> {
    let device = peer_target(s, device_id).await?;
    #[derive(Deserialize)]
    struct Listing {
        root: RootName,
    }
    #[derive(Deserialize)]
    struct RootName {
        name: String,
        read_only: bool,
    }
    let url = format!("/api/v1/files?root_id={}&path={}&limit=1", percent_encode(root_id), percent_encode(path));
    let (listing, _): (Listing, _) = s
        .peers
        .call(&device, Method::GET, &url, None::<&()>, Some(Acting::of(user)), CALL_TIMEOUT)
        .await
        .map_err(|e| explain(e, &device.name))?;
    if listing.root.read_only {
        return Err(NexusError::invalid(format!("{} is read-only on {}", listing.root.name, device.name)));
    }
    Ok(if path.is_empty() { listing.root.name } else { format!("{} / {path}", listing.root.name) })
}

pub async fn set(State(s): State<SharedState>, user: AuthUser, Json(req): Json<SetBackup>) -> ApiResult<Json<BackupStatus>> {
    require_phone_person(&user)?;
    if s.media_dir.is_none() {
        return Err(NexusError::invalid("Only phones back up their photos").into());
    }
    let mut sources: Vec<String> = req.sources.into_iter().filter(|src| SOURCES.iter().any(|(k, _)| k == src)).collect();
    sources.dedup();
    if sources.is_empty() {
        return Err(NexusError::invalid("Choose what to back up").into());
    }
    let path = paths::normalize_relative(&req.path)?;
    let label = check_destination(&s, &user, &req.device_id, &req.root_id, &path).await?;
    let backup = s
        .db
        .set_backup(&NewBackup {
            device_id: &req.device_id,
            root_id: &req.root_id,
            path: &path,
            label: &label,
            sources: &sources,
            enabled: req.enabled,
            created_by: &user.username,
        })
        .await?;
    audit(&s, &user, "backup.set", Some(&label)).await;
    if backup.enabled {
        s.backup.run_now();
    }
    Ok(Json(status_of(&s).await?))
}

#[derive(Deserialize)]
pub struct UpdateBackup {
    enabled: bool,
}

pub async fn update(State(s): State<SharedState>, user: AuthUser, Json(req): Json<UpdateBackup>) -> ApiResult<Json<BackupStatus>> {
    require_phone_person(&user)?;
    let backup = s.db.get_backup().await?.ok_or_else(|| NexusError::NotFound("Backup".into()))?;
    s.db.set_backup_enabled(&backup.id, req.enabled).await?;
    if req.enabled {
        s.backup.run_now();
    }
    Ok(Json(status_of(&s).await?))
}

pub async fn remove(State(s): State<SharedState>, user: AuthUser) -> ApiResult<StatusCode> {
    require_phone_person(&user)?;
    s.db.delete_backup().await?;
    audit(&s, &user, "backup.removed", None).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn run(State(s): State<SharedState>, user: AuthUser) -> ApiResult<StatusCode> {
    require_phone_person(&user)?;
    s.db.get_backup().await?.ok_or_else(|| NexusError::NotFound("Backup".into()))?;
    s.backup.run_now();
    Ok(StatusCode::ACCEPTED)
}

// ---------------------------------------------------------------------------
// Files shared to the app

#[derive(Serialize)]
pub struct SharedFile {
    name: String,
    size: u64,
}

fn inbox(s: &AppState) -> Result<&Path> {
    s.share_inbox.as_deref().ok_or_else(|| NexusError::NotFound("Shared files".into()))
}

/// The files waiting in the inbox, oldest first. Files still being copied
/// in end with `.part`.
fn waiting(dir: &Path) -> Vec<(SharedFile, i64)> {
    let mut out: Vec<(SharedFile, i64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            (meta.is_file() && !name.starts_with('.') && !name.ends_with(".part"))
                .then(|| (SharedFile { name, size: meta.len() }, modified_ms(&meta)))
        })
        .collect();
    out.sort_by_key(|(_, at)| *at);
    out
}

pub async fn shared(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<SharedFile>>> {
    user.require_person()?;
    let Ok(dir) = inbox(&s) else { return Ok(Json(Vec::new())) };
    let dir = dir.to_path_buf();
    let list = tokio::task::spawn_blocking(move || waiting(&dir)).await.map_err(NexusError::internal)?;
    Ok(Json(list.into_iter().map(|(f, _)| f).collect()))
}

/// A file in the inbox by its plain name.
fn inbox_file(dir: &Path, name: &str) -> Result<PathBuf> {
    paths::validate_name(name)?;
    if name.starts_with('.') || name.ends_with(".part") {
        return Err(NexusError::NotFound("Shared file".into()));
    }
    let path = dir.join(name);
    if !path.is_file() {
        return Err(NexusError::NotFound(format!("The shared file {name}")));
    }
    Ok(path)
}

#[derive(Deserialize)]
pub struct SendShared {
    names: Vec<String>,
    device_id: String,
    root_id: String,
    #[serde(default)]
    path: String,
}

/// `POST /shared/send`: sends shared files to a folder on a paired device in
/// the background, and removes each from the inbox once it arrived.
pub async fn send_shared(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(req): Json<SendShared>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    user.require_person()?;
    user.require(Action::Write)?;
    let dir = inbox(&s)?.to_path_buf();
    if req.names.is_empty() {
        return Err(NexusError::invalid("Choose something to send").into());
    }
    let files: Vec<(String, PathBuf, u64)> = req
        .names
        .iter()
        .map(|n| {
            let p = inbox_file(&dir, n)?;
            let size = std::fs::metadata(&p)?.len();
            Ok((n.clone(), p, size))
        })
        .collect::<Result<_>>()?;
    let path = paths::normalize_relative(&req.path)?;
    let target = peer_target(&s, &req.device_id).await?;
    let title = if files.len() == 1 { format!("Send {}", files[0].0) } else { format!("Send {} files", files.len()) };
    let job = s.jobs.add("copy", title, format!("This phone → {}", target.name), user.username.clone());
    job.update(|i| {
        i.files_total = files.len() as u64;
        i.bytes_total = files.iter().map(|f| f.2).sum();
    });
    let id = job.id.clone();
    let state = s.clone();
    tokio::spawn(async move {
        let s = &*state;
        let Some(_slot) = s.jobs.start(s, &job).await else { return };
        let side = Side::Peer(target);
        let mover = Mover { s, user: &user };
        for (name, file, size) in files {
            if job.is_cancelled() {
                break;
            }
            job.update(|i| i.current = Some(name.clone()));
            let req = NewUpload {
                root_id: req.root_id.clone(),
                path: path.clone(),
                name: name.clone(),
                size,
                on_conflict: Some("rename".into()),
                modified_at: None,
                make_parents: false,
            };
            match mover.send_disk_file(&job, &file, &side, req).await {
                Ok(_) => {
                    let _ = tokio::fs::remove_file(&file).await;
                    job.update(|i| i.files_done += 1);
                }
                Err(e) => job.fail_file(&name, &e),
            }
        }
        finish_job(s, &job);
    });
    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({ "id": id }))))
}

#[derive(Deserialize)]
pub struct DiscardShared {
    /// Files to drop; all of them when empty.
    #[serde(default)]
    names: Vec<String>,
}

pub async fn discard_shared(State(s): State<SharedState>, user: AuthUser, Json(req): Json<DiscardShared>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::Write)?;
    let dir = inbox(&s)?.to_path_buf();
    let names: Vec<String> = if req.names.is_empty() {
        let d = dir.clone();
        tokio::task::spawn_blocking(move || waiting(&d)).await.map_err(NexusError::internal)?.into_iter().map(|(f, _)| f.name).collect()
    } else {
        req.names
    };
    for name in names {
        if let Ok(p) = inbox_file(&dir, &name) {
            let _ = tokio::fs::remove_file(p).await;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_skips_hidden_recent_and_empty_files() {
        let media = std::env::temp_dir().join(format!("nexus-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&media);
        std::fs::create_dir_all(media.join("DCIM/Camera")).unwrap();
        std::fs::create_dir_all(media.join("DCIM/.thumbnails")).unwrap();
        std::fs::create_dir_all(media.join("Download")).unwrap();
        std::fs::write(media.join("DCIM/Camera/IMG_1.jpg"), b"photo").unwrap();
        std::fs::write(media.join("DCIM/Camera/.pending-1-IMG_2.jpg"), b"half").unwrap();
        std::fs::write(media.join("DCIM/Camera/empty.jpg"), b"").unwrap();
        std::fs::write(media.join("DCIM/.thumbnails/1.jpg"), b"thumb").unwrap();
        std::fs::write(media.join("Download/other.pdf"), b"pdf").unwrap();

        let later = now_ms() + SETTLE_MS + 1000;
        let found = scan(&media, &["DCIM".into(), "Movies".into()], later).unwrap();
        assert_eq!(found.iter().map(|f| f.rel.as_str()).collect::<Vec<_>>(), ["DCIM/Camera/IMG_1.jpg"]);
        assert_eq!(found[0].size, 5);
        // Just written: still settling.
        assert!(scan(&media, &["DCIM".into()], now_ms()).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&media);
    }
}
