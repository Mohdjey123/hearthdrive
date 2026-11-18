//! Share links: anyone with the link can download a file or folder from
//! anywhere, until it expires. See [`crate::share`] for the services.
//!
//! The upload runs as a job on the Transfers page. Folders are sent as one
//! zip file. Passwords are never stored here: they are turned into a key
//! that only the service keeps.

use super::audit;
use super::files::tree_of;
use super::transfers::{Job, finish_job};
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::share::{self, SendFile, Watch};
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use nexus_common::time::now_ms;
use nexus_common::types::{FileEntry, Role};
use nexus_common::{NexusError, Result};
use nexus_database::{NewShare, Share};
use nexus_security::Action;
use nexus_storage::paths;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const SEND_URL_SETTING: &str = "share.send_url";

pub async fn send_url(s: &AppState) -> String {
    s.db.get_setting(SEND_URL_SETTING).await.ok().flatten().unwrap_or_else(|| share::DEFAULT_SEND.to_string())
}

/// Owners and administrators see every link; everyone else their own.
fn sees_all(user: &AuthUser) -> bool {
    matches!(user.role, Role::Owner | Role::Administrator) && !user.is_limited()
}

fn visible(user: &AuthUser, share: &Share) -> bool {
    sees_all(user) || share.owner_key == user.owner_key()
}

// ---------------------------------------------------------------------------
// Listing and settings

/// A link, with upload progress while it is uploading.
#[derive(Serialize)]
pub struct ShareView {
    #[serde(flatten)]
    share: Share,
    bytes_done: Option<u64>,
    bytes_total: Option<u64>,
}

pub async fn list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<ShareView>>> {
    user.require(Action::Write)?;
    s.db.expire_shares().await?;
    let key = user.owner_key();
    let shares = s.db.list_shares(if sees_all(&user) { None } else { Some(&key) }).await?;
    let views = shares
        .into_iter()
        .filter(|sh| user.sees_root(&sh.root_id))
        .map(|share| {
            let progress = (share.status == "uploading")
                .then(|| share.job_id.as_deref().and_then(|j| s.jobs.find(j)))
                .flatten()
                .map(|job| job.progress());
            ShareView { share, bytes_done: progress.map(|p| p.0), bytes_total: progress.map(|p| p.1) }
        })
        .collect();
    Ok(Json(views))
}

#[derive(Serialize)]
pub struct Services {
    send_url: String,
    /// `None` when the Send service could not be reached.
    send: Option<share::SendLimits>,
    send_error: Option<String>,
    tempsh_max_size: u64,
    tempsh_expire_secs: u64,
    /// Whether the caller may change the Send service.
    can_configure: bool,
}

/// `GET /shares/services`: the link services and what they allow.
pub async fn services(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Services>> {
    user.require(Action::Write)?;
    let send_url = send_url(&s).await;
    let (send, send_error) = match share::send_limits(&send_url).await {
        Ok(l) => (Some(l), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Ok(Json(Services {
        send_url,
        send,
        send_error,
        tempsh_max_size: share::TEMPSH_MAX_SIZE,
        tempsh_expire_secs: share::TEMPSH_EXPIRE_SECS,
        can_configure: user.require(Action::ManageStorage).is_ok() && user.require_person().is_ok(),
    }))
}

#[derive(Deserialize)]
pub struct ServiceSettings {
    send_url: String,
}

/// `PUT /shares/services`: which Send service to use (owners and
/// administrators). It must answer like one before it is saved.
pub async fn set_services(State(s): State<SharedState>, user: AuthUser, Json(req): Json<ServiceSettings>) -> ApiResult<Json<Services>> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    let url = if req.send_url.trim().is_empty() { share::DEFAULT_SEND.to_string() } else { share::service_url(&req.send_url)? };
    share::send_limits(&url).await?;
    s.db.set_setting(SEND_URL_SETTING, &url).await?;
    audit(&s, &user, "share.service.changed", Some(&url)).await;
    services(State(s), user).await
}

// ---------------------------------------------------------------------------
// Making a link

#[derive(Deserialize)]
pub struct NewLink {
    file_id: String,
    /// `send` (default) or `tempsh`.
    #[serde(default)]
    provider: Option<String>,
    /// Send only; temp.sh always keeps files for 3 days.
    #[serde(default)]
    expires_secs: Option<u64>,
    #[serde(default)]
    downloads: Option<u32>,
    #[serde(default)]
    password: Option<String>,
}

/// `POST /shares`: uploads a file or folder and makes a link to it. Answers
/// at once; the link appears when the upload finishes.
pub async fn create(State(s): State<SharedState>, user: AuthUser, Json(req): Json<NewLink>) -> ApiResult<(StatusCode, Json<Share>)> {
    user.require(Action::Read)?;
    user.require(Action::Write)?;
    let file = s.db.get_file(&req.file_id).await?;
    user.require_root(&file.root_id)?;
    let root = s.db.get_root(&file.root_id).await?;
    s.storage.ensure_online(&root)?;
    if !file.is_dir && file.size <= 0 {
        return Err(NexusError::invalid("This file is empty").into());
    }

    let tempsh = match req.provider.as_deref().unwrap_or("send") {
        "send" => false,
        "tempsh" => true,
        _ => return Err(NexusError::invalid("Choose Send or temp.sh").into()),
    };
    let password = req.password.filter(|p| !p.is_empty());
    let (service, expires_secs, downloads) = if tempsh {
        if password.is_some() {
            return Err(NexusError::invalid("temp.sh links cannot have a password").into());
        }
        if file.size as u64 > share::TEMPSH_MAX_SIZE {
            return Err(NexusError::invalid("This is larger than temp.sh accepts (4 GB)").into());
        }
        (share::TEMPSH.to_string(), share::TEMPSH_EXPIRE_SECS, None)
    } else {
        let url = send_url(&s).await;
        let limits = share::send_limits(&url).await?;
        if file.size as u64 > limits.max_file_size {
            return Err(NexusError::invalid(format!("This is larger than the Send service accepts ({})", human_size(limits.max_file_size))).into());
        }
        if password.as_ref().is_some_and(|p| p.len() > 256) {
            return Err(NexusError::invalid("The password is too long").into());
        }
        let expires = req.expires_secs.unwrap_or(86_400).clamp(60, limits.max_expire_seconds);
        let downloads = req.downloads.unwrap_or(1).clamp(1, limits.max_downloads.max(1));
        (url, expires, Some(downloads))
    };

    let name = if file.is_dir { format!("{}.zip", file.filename) } else { file.filename.clone() };
    let service_name = if tempsh { "temp.sh" } else { "Send" };
    let job = s.jobs.add("share", format!("Share {name}"), format!("This computer → {service_name}"), user.username.clone());
    let record = s
        .db
        .create_share(&NewShare {
            file_id: &file.id,
            root_id: &file.root_id,
            name: &name,
            is_dir: file.is_dir,
            size: file.size,
            provider: if tempsh { "tempsh" } else { "send" },
            service: &service,
            expires_at: now_ms() + expires_secs as i64 * 1000,
            downloads: downloads.map(i64::from),
            has_password: password.is_some(),
            job_id: &job.id,
            owner_key: &user.owner_key(),
            created_by: &user.username,
        })
        .await?;

    let state = s.clone();
    let share = record.clone();
    tokio::spawn(async move {
        let run = Run { s: &state, job: &job, share: &share, file: &file, expires_secs, downloads, password: password.as_deref() };
        let result = run.go().await;
        let (status, error) = match &result {
            Ok(()) => ("active", None),
            Err(_) if job.is_cancelled() => ("failed", Some("Cancelled".to_string())),
            Err(e) => ("failed", Some(e.to_string())),
        };
        if status != "active" {
            let _ = state.db.set_share_status(&share.id, status, error.as_deref()).await;
            if let Some(e) = &error {
                job.update(|i| i.errors.push(format!("{}: {e}", share.name)));
            }
        }
        // The Shares page follows the job's own updates.
        finish_job(&state, &job);
        if result.is_ok() {
            audit(&state, &user, "share.created", Some(&format!("{} via {service_name}", share.name))).await;
        }
    });
    Ok((StatusCode::ACCEPTED, Json(record)))
}

fn human_size(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 { format!("{gb:.1} GB") } else { format!("{:.0} MB", bytes as f64 / 1e6) }
}

/// Reports upload progress into the job.
struct JobWatch(Arc<Job>);

impl Watch for JobWatch {
    fn sent(&self, bytes: u64) {
        self.0.set_bytes(bytes);
    }
    fn cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct Run<'a> {
    s: &'a AppState,
    job: &'a Arc<Job>,
    share: &'a Share,
    file: &'a FileEntry,
    expires_secs: u64,
    downloads: Option<u32>,
    password: Option<&'a str>,
}

/// A zip file made for a folder, removed when dropped.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Run<'_> {
    async fn go(&self) -> Result<()> {
        let Some(_slot) = self.s.jobs.start(self.s, self.job).await else {
            return Err(NexusError::Unavailable("Cancelled".into()));
        };
        let root = self.s.db.get_root(&self.file.root_id).await?;
        self.s.storage.ensure_online(&root)?;

        let (_temp, path, size, mime) = if self.file.is_dir {
            self.job.update(|i| i.current = Some(format!("Packing {}", self.share.name)));
            let (temp, size) = self.zip_folder(&root).await?;
            let path = temp.0.clone();
            (Some(temp), path, size, "application/zip".to_string())
        } else {
            let path = self.s.storage.resolve(&root, &self.file.relative_path)?;
            let size = tokio::fs::metadata(&path).await?.len();
            let mime = self.file.mime_type.clone().unwrap_or_else(|| "application/octet-stream".into());
            (None, path, size, mime)
        };
        if size == 0 {
            return Err(NexusError::invalid("There is nothing to share"));
        }
        self.job.update(|i| {
            i.files_total = 1;
            i.bytes_total = size;
            i.current = Some(self.share.name.clone());
        });

        let watch = Arc::new(JobWatch(self.job.clone()));
        let (url, remote_id, owner_token) = if self.share.provider == "tempsh" {
            if size > share::TEMPSH_MAX_SIZE {
                return Err(NexusError::invalid("This is larger than temp.sh accepts (4 GB)"));
            }
            let link = share::tempsh_upload(&path, size, &self.share.name, &mime, watch).await?;
            (link, None, None)
        } else {
            let file = SendFile {
                path: &path,
                size,
                name: &self.share.name,
                mime: &mime,
                expire_secs: self.expires_secs,
                downloads: self.downloads.unwrap_or(1),
                password: self.password,
            };
            let up = share::send_upload(&self.share.service, &file, watch.as_ref()).await?;
            (up.url, Some(up.id), Some(up.owner_token))
        };

        // Deleted while it was uploading: take it down again.
        if self.s.db.get_share(&self.share.id).await.is_err() {
            if let (Some(id), Some(token)) = (&remote_id, &owner_token) {
                let _ = share::send_delete(&self.share.service, id, token).await;
            }
            return Err(NexusError::Unavailable("Cancelled".into()));
        }
        self.s.db.share_ready(&self.share.id, &url, remote_id.as_deref(), owner_token.as_deref(), size as i64).await?;
        self.job.update(|i| i.files_done = 1);
        Ok(())
    }

    /// Packs a folder into a zip file in the node's data folder.
    async fn zip_folder(&self, root: &nexus_common::types::StorageRoot) -> Result<(TempFile, u64)> {
        let items = tree_of(self.s, &root.id, &self.file.relative_path).await?;
        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            let rel = paths::join(&self.file.relative_path, &item.path);
            let disk = if item.is_dir { None } else { Some(self.s.storage.resolve(root, &rel)?) };
            entries.push((format!("{}/{}", self.file.filename, item.path), disk));
        }
        let dir = self.s.data_dir.join("tmp");
        tokio::fs::create_dir_all(&dir).await?;
        let temp = TempFile(dir.join(format!("share-{}.zip", self.share.id)));
        let out = temp.0.clone();
        let job = self.job.clone();
        let top = self.file.filename.clone();
        let size = tokio::task::spawn_blocking(move || write_zip(&out, &top, entries, &job))
            .await
            .map_err(NexusError::internal)??;
        Ok((temp, size))
    }
}

/// Files that are already compressed are stored as they are.
fn compressed_already(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "avif" | "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v"
            | "mp3" | "m4a" | "aac" | "ogg" | "opus" | "flac" | "zip" | "7z" | "rar" | "gz" | "xz" | "bz2" | "zst" | "pdf"
            | "docx" | "xlsx" | "pptx" | "apk"
    )
}

fn write_zip(out: &std::path::Path, top: &str, entries: Vec<(String, Option<PathBuf>)>, job: &Job) -> Result<u64> {
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;
    let file = std::fs::File::create(out)?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let zip_err = |e: zip::result::ZipError| NexusError::internal(format!("zip: {e}"));
    zip.add_directory(format!("{top}/"), SimpleFileOptions::default()).map_err(zip_err)?;
    for (name, disk) in entries {
        if job.is_cancelled() {
            return Err(NexusError::Unavailable("Cancelled".into()));
        }
        match disk {
            None => zip.add_directory(format!("{name}/"), SimpleFileOptions::default()).map_err(zip_err)?,
            Some(path) => {
                let Ok(mut src) = std::fs::File::open(&path) else {
                    job.update(|i| i.errors.push(format!("{name}: could not be read; left out")));
                    continue;
                };
                let len = src.metadata().map(|m| m.len()).unwrap_or(0);
                let method = if compressed_already(&name) { CompressionMethod::Stored } else { CompressionMethod::Deflated };
                let options = SimpleFileOptions::default().compression_method(method).large_file(len >= u32::MAX as u64);
                zip.start_file(name, options).map_err(zip_err)?;
                std::io::copy(&mut src, &mut zip)?;
            }
        }
    }
    let mut inner = zip.finish().map_err(zip_err)?;
    std::io::Write::flush(&mut inner)?;
    drop(inner);
    Ok(std::fs::metadata(out)?.len())
}

// ---------------------------------------------------------------------------
// Taking links down

/// `DELETE /shares/{id}`: deletes the file from the service and forgets the
/// link. A link still uploading is cancelled.
pub async fn remove(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require(Action::Write)?;
    let share = s.db.get_share(&id).await?;
    if !visible(&user, &share) || !user.sees_root(&share.root_id) {
        return Err(NexusError::NotFound("Link".into()).into());
    }
    if share.status == "uploading"
        && let Some(job) = share.job_id.as_deref().and_then(|j| s.jobs.find(j))
    {
        job.cancel();
    }
    if share.status == "active"
        && let (Some(remote), Some(token)) = (&share.remote_id, &share.owner_token)
    {
        share::send_delete(&share.service, remote, token).await?;
    }
    s.db.forget_share(&id).await?;
    audit(&s, &user, "share.deleted", Some(&share.name)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /shares/refresh`: asks Send how each link stands (downloads used,
/// time left) and marks the ones that stopped working.
pub async fn refresh(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<ShareView>>> {
    user.require(Action::Write)?;
    s.db.expire_shares().await?;
    let key = user.owner_key();
    let shares = s.db.list_shares(if sees_all(&user) { None } else { Some(&key) }).await?;
    for sh in shares.iter().filter(|sh| sh.status == "active" && sh.provider == "send") {
        let (Some(remote), Some(token)) = (&sh.remote_id, &sh.owner_token) else { continue };
        match share::send_info(&sh.service, remote, token).await {
            Ok(Some(info)) => {
                s.db.set_share_usage(&sh.id, info.dl as i64, now_ms() + info.ttl).await?;
            }
            // Expired, or every download was used.
            Ok(None) => s.db.set_share_status(&sh.id, "expired", None).await?,
            Err(e) => {
                tracing::debug!(error = %e, "could not check a share link");
                break;
            }
        }
    }
    list(State(s), user).await
}

/// At start-up: uploads cut off by a restart failed. Every hour: links whose
/// time ran out are marked expired.
pub fn spawn(s: SharedState) {
    tokio::spawn(async move {
        if let Err(e) = s.db.fail_unfinished_shares().await {
            tracing::warn!(error = %e, "could not tidy share links");
        }
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let _ = s.db.expire_shares().await;
        }
    });
}
