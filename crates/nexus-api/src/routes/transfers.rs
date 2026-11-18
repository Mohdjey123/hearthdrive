//! Copies between devices, run as background jobs.
//!
//! The source and the destination can each be this computer or a paired
//! device. Content streams straight through this node; nothing is buffered
//! in memory or kept here. Every file goes through a resumable upload, so a
//! dropped connection costs only a retry from where it stopped.
//!
//! [`Mover`] holds the operations on either side; sync uses them too.

use super::audit;
use super::files::{TreeItem, TreeResponse, percent_encode, tree_of};
use super::uploads::{self, NewUpload, UploadProgress};
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::peers::{Acting, CALL_TIMEOUT, TRANSFER_TIMEOUT, Target, peer_error, read_body};
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use futures_util::StreamExt;
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::types::FileEntry;
use nexus_common::{NexusError, Result, new_id};
use nexus_security::Action;
use nexus_storage::paths;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// How often one file is tried again after its connection drops.
const RETRIES: u32 = 8;
/// Folder listings can be large.
const MAX_TREE: usize = 256 * 1024 * 1024;
const TREE_TIMEOUT: Duration = Duration::from_secs(120);
/// Finished jobs kept for the Transfers page.
const KEEP_FINISHED: usize = 100;

// ---------------------------------------------------------------------------
// Jobs

#[derive(Debug, Clone, Default, Serialize)]
pub struct JobInfo {
    /// "Copy Photos", "Sync Documents"
    pub title: String,
    /// `queued` | `running` | `done` | `failed` | `cancelled`
    pub status: String,
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_total: u64,
    /// The file being sent right now.
    pub current: Option<String>,
    /// Files that could not be sent, with the reason.
    pub errors: Vec<String>,
    /// Times a connection dropped and the transfer continued.
    pub retries: u32,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

pub struct Job {
    pub id: String,
    /// `copy` | `sync` | `backup` | `share`
    pub kind: &'static str,
    /// "This computer → Laptop"
    pub route: String,
    pub by: String,
    pub created_at: i64,
    bytes_done: AtomicU64,
    cancelled: AtomicBool,
    info: Mutex<JobInfo>,
}

#[derive(Serialize)]
pub struct JobView {
    id: String,
    kind: &'static str,
    route: String,
    by: String,
    created_at: i64,
    bytes_done: u64,
    #[serde(flatten)]
    info: JobInfo,
}

impl Job {
    fn view(&self) -> JobView {
        JobView {
            id: self.id.clone(),
            kind: self.kind,
            route: self.route.clone(),
            by: self.by.clone(),
            created_at: self.created_at,
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            info: self.info.lock().unwrap().clone(),
        }
    }

    pub fn update(&self, f: impl FnOnce(&mut JobInfo)) {
        f(&mut self.info.lock().unwrap());
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn bytes(&self) -> u64 {
        self.bytes_done.load(Ordering::Relaxed)
    }

    pub(crate) fn set_bytes(&self, n: u64) {
        self.bytes_done.store(n, Ordering::Relaxed);
    }

    /// Bytes sent so far and in total.
    pub(crate) fn progress(&self) -> (u64, u64) {
        (self.bytes(), self.info.lock().unwrap().bytes_total)
    }

    /// Asks the job to stop; it notices at its next step.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn finished(&self) -> bool {
        matches!(self.info.lock().unwrap().status.as_str(), "done" | "failed" | "cancelled")
    }

    pub fn error_count(&self) -> usize {
        self.info.lock().unwrap().errors.len()
    }

    /// Records one file that could not be sent.
    pub fn fail_file(&self, name: &str, e: &NexusError) {
        self.update(|i| i.errors.push(format!("{name}: {e}")));
    }
}

/// Copy and sync jobs on this node, newest first.
pub struct Jobs {
    list: Mutex<Vec<Arc<Job>>>,
    /// Jobs running at the same time; the rest wait their turn.
    slots: tokio::sync::Semaphore,
}

impl Default for Jobs {
    fn default() -> Self {
        Self { list: Mutex::new(Vec::new()), slots: tokio::sync::Semaphore::new(2) }
    }
}

impl Jobs {
    pub fn add(&self, kind: &'static str, title: String, route: String, by: String) -> Arc<Job> {
        let job = Arc::new(Job {
            id: new_id(),
            kind,
            route,
            by,
            created_at: now_ms(),
            bytes_done: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
            info: Mutex::new(JobInfo { status: "queued".into(), title, ..Default::default() }),
        });
        let mut list = self.list.lock().unwrap();
        list.insert(0, job.clone());
        let mut finished = 0;
        list.retain(|j| !j.finished() || {
            finished += 1;
            finished <= KEEP_FINISHED
        });
        job
    }

    /// Jobs the caller may see: all of them, or their own if they are
    /// limited to some storage.
    fn views(&self, user: &AuthUser) -> Vec<JobView> {
        self.list.lock().unwrap().iter().filter(|j| !user.is_limited() || j.by == user.username).map(|j| j.view()).collect()
    }

    pub(crate) fn find(&self, id: &str) -> Option<Arc<Job>> {
        self.list.lock().unwrap().iter().find(|j| j.id == id).cloned()
    }

    /// Forgets finished jobs: the caller's own if they are limited to some
    /// storage (they cannot see anyone else's).
    fn clear_finished(&self, user: &AuthUser) {
        self.list.lock().unwrap().retain(|j| !j.finished() || (user.is_limited() && j.by != user.username));
    }

    /// Waits for a free slot. Returns `None` if the job was cancelled meanwhile.
    pub async fn start(&self, s: &AppState, job: &Job) -> Option<tokio::sync::SemaphorePermit<'_>> {
        let permit = self.slots.acquire().await.ok()?;
        if job.is_cancelled() {
            set_status(s, job, "cancelled");
            return None;
        }
        set_status(s, job, "running");
        Some(permit)
    }
}

pub fn set_status(s: &AppState, job: &Job, status: &str) {
    job.update(|i| {
        i.status = status.to_string();
        match status {
            "running" => i.started_at = Some(now_ms()),
            "done" | "failed" | "cancelled" => {
                i.finished_at = Some(now_ms());
                i.current = None;
            }
            _ => {}
        }
    });
    s.events.emit(Event::TransferUpdated { id: job.id.clone(), status: status.to_string() });
}

/// Ends a job: cancelled, failed (nothing went through) or done.
pub fn finish_job(s: &AppState, job: &Job) {
    let (done, errors) = {
        let i = job.info.lock().unwrap();
        (i.files_done, i.errors.len())
    };
    let status = if job.is_cancelled() {
        "cancelled"
    } else if errors > 0 && done == 0 {
        "failed"
    } else {
        "done"
    };
    set_status(s, job, status);
}

// ---------------------------------------------------------------------------
// Moving files between this node and paired devices

/// Where a file lives or goes.
#[derive(Clone)]
pub enum Side {
    Local,
    Peer(Target),
}

impl Side {
    pub fn name(&self) -> String {
        match self {
            Side::Local => "this computer".into(),
            Side::Peer(t) => t.name.clone(),
        }
    }
}

/// What a copy or sync needs to know about a file.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub id: String,
    pub root_id: String,
    pub relative_path: String,
    pub filename: String,
    pub is_dir: bool,
    pub size: i64,
    pub modified_at: Option<i64>,
    #[serde(default)]
    pub content_hash: Option<String>,
}

impl From<FileEntry> for Meta {
    fn from(f: FileEntry) -> Self {
        Self {
            id: f.id,
            root_id: f.root_id,
            relative_path: f.relative_path,
            filename: f.filename,
            is_dir: f.is_dir,
            size: f.size,
            modified_at: f.modified_at,
            content_hash: f.content_hash,
        }
    }
}

impl Meta {
    /// A folder tree entry, under a folder at `base` in `root_id`.
    pub fn of_tree(item: &TreeItem, root_id: &str, base: &str) -> Self {
        let (_, name) = paths::split_parent(&item.path);
        Self {
            id: item.id.clone(),
            root_id: root_id.to_string(),
            relative_path: paths::join(base, &item.path),
            filename: name,
            is_dir: item.is_dir,
            size: item.size,
            modified_at: item.modified_at,
            content_hash: item.content_hash.clone(),
        }
    }
}

pub async fn peer_target(s: &AppState, device_id: &str) -> Result<Target> {
    let device = s.db.get_device(device_id).await?;
    if device.is_local || device.public_key.is_none() {
        return Err(NexusError::NotFound("paired device".into()));
    }
    Target::of(&device)
}

/// Errors worth another try: the connection dropped, the device is busy,
/// or the other side has not noticed the last attempt ended yet.
pub fn retryable(e: &NexusError) -> bool {
    matches!(e, NexusError::PeerUnreachable(_) | NexusError::Io(_) | NexusError::Conflict(_) | NexusError::Unavailable(_))
}

fn cancelled() -> NexusError {
    NexusError::Unavailable("Cancelled".into())
}

/// Reads a local file from `offset` as a stream of chunks.
fn file_body(file: tokio::fs::File) -> Body {
    Body::from_stream(futures_util::stream::unfold(file, |mut file| async move {
        let mut buf = vec![0u8; 256 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(Bytes::from(buf)), file))
            }
            Err(e) => Some((Err(e), file)),
        }
    }))
}

/// Counts bytes into the job as they pass, and stops when it is cancelled.
fn counted(body: Body, job: Arc<Job>) -> Body {
    Body::from_stream(body.into_data_stream().map(move |chunk| {
        if job.is_cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        let chunk = chunk.map_err(std::io::Error::other)?;
        job.bytes_done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        Ok(chunk)
    }))
}

/// What a file is sent from.
enum Source<'a> {
    /// A file in a storage location on either side, by id.
    Item(&'a Side, &'a str),
    /// A file on this device's disk outside any storage location.
    Disk(&'a std::path::Path),
}

/// Runs file operations on either side for one person.
pub struct Mover<'a> {
    pub s: &'a AppState,
    pub user: &'a AuthUser,
}

impl Mover<'_> {
    fn acting(&self) -> Option<Acting<'_>> {
        Some(Acting::of(self.user))
    }

    async fn json<T: DeserializeOwned>(&self, t: &Target, m: Method, path: &str, body: Option<&impl Serialize>, timeout: Duration) -> Result<T> {
        Ok(self.s.peers.call(t, m, path, body, self.acting(), timeout).await?.0)
    }

    pub async fn info(&self, side: &Side, file_id: &str) -> Result<Meta> {
        match side {
            Side::Local => {
                self.user.require(Action::Read).map_err(|e| e.0)?;
                let file = self.s.db.get_file(file_id).await?;
                self.user.require_root(&file.root_id).map_err(|e| e.0)?;
                Ok(file.into())
            }
            Side::Peer(t) => self.json(t, Method::GET, &format!("/api/v1/files/{file_id}"), None::<&()>, CALL_TIMEOUT).await,
        }
    }

    /// Everything under a folder, paths relative to it, parents first.
    pub async fn tree(&self, side: &Side, root_id: &str, path: &str) -> Result<Vec<TreeItem>> {
        match side {
            Side::Local => {
                self.user.require(Action::Read).map_err(|e| e.0)?;
                self.user.require_root(root_id).map_err(|e| e.0)?;
                tree_of(self.s, root_id, path).await
            }
            Side::Peer(t) => {
                let url = format!("/api/v1/files/tree?root_id={}&path={}", percent_encode(root_id), percent_encode(path));
                let (reply, _): (TreeResponse, _) =
                    self.s.peers.call_limited(t, Method::GET, &url, None::<&()>, self.acting(), TREE_TIMEOUT, MAX_TREE).await?;
                Ok(reply.items)
            }
        }
    }

    /// Creates a folder. `Ok(false)` if something with that name exists.
    pub async fn mkdir(&self, side: &Side, root_id: &str, parent: &str, name: &str) -> Result<bool> {
        let made = match side {
            Side::Local => {
                self.user.require(Action::Write).map_err(|e| e.0)?;
                self.user.require_root(root_id).map_err(|e| e.0)?;
                match self.s.storage.create_folder(root_id, parent, name).await {
                    Ok((root, rel)) => {
                        self.s.indexer.reindex_path(&root, &rel).await?;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            Side::Peer(t) => self
                .json::<serde_json::Value>(
                    t,
                    Method::POST,
                    "/api/v1/files/folder",
                    Some(&json!({ "root_id": root_id, "path": parent, "name": name })),
                    CALL_TIMEOUT,
                )
                .await
                .map(|_| ()),
        };
        match made {
            Ok(()) => Ok(true),
            Err(NexusError::Conflict(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Creates a folder under a free name ("Photos", "Photos (1)", ...).
    pub async fn mkdir_free(&self, side: &Side, root_id: &str, parent: &str, name: &str) -> Result<String> {
        for n in 0..1000 {
            let candidate = paths::numbered_name(name, n, true);
            if self.mkdir(side, root_id, parent, &candidate).await? {
                return Ok(candidate);
            }
        }
        Err(NexusError::Conflict("Too many folders with this name".into()))
    }

    /// Moves a file or folder to the trash on its device.
    pub async fn trash(&self, side: &Side, file_id: &str) -> Result<()> {
        match side {
            Side::Local => {
                self.user.require(Action::Write).map_err(|e| e.0)?;
                let file = self.s.db.get_file(file_id).await?;
                self.user.require_root(&file.root_id).map_err(|e| e.0)?;
                self.s.storage.trash(&file, self.user.user_id.as_deref()).await?;
                self.s.events.emit(Event::FileDeleted { root_id: file.root_id.clone(), path: file.relative_path.clone() });
                audit(self.s, self.user, "file.trashed", Some(&file.relative_path)).await;
                Ok(())
            }
            Side::Peer(t) => self
                .json::<serde_json::Value>(t, Method::DELETE, &format!("/api/v1/files/{file_id}"), None::<&()>, CALL_TIMEOUT)
                .await
                .map(|_| ()),
        }
    }

    /// Opens a file's content from `offset`.
    async fn open(&self, side: &Side, file_id: &str, offset: u64) -> Result<Body> {
        match side {
            Side::Local => {
                self.user.require(Action::Read).map_err(|e| e.0)?;
                let file = self.s.db.get_file(file_id).await?;
                self.user.require_root(&file.root_id).map_err(|e| e.0)?;
                if file.is_dir {
                    return Err(NexusError::invalid("This is a folder"));
                }
                let root = self.s.db.get_root(&file.root_id).await?;
                self.s.storage.ensure_online(&root)?;
                let path = self.s.storage.resolve(&root, &file.relative_path)?;
                let mut handle = tokio::fs::File::open(&path).await?;
                if offset > 0 {
                    handle.seek(std::io::SeekFrom::Start(offset)).await?;
                }
                Ok(file_body(handle))
            }
            Side::Peer(t) => {
                let link: serde_json::Value =
                    self.json(t, Method::POST, &format!("/api/v1/files/{file_id}/link"), None::<&()>, CALL_TIMEOUT).await?;
                let url = link["url"]
                    .as_str()
                    .ok_or_else(|| NexusError::PeerUnreachable(format!("{} sent an unexpected reply", t.name)))?;
                let mut headers = HeaderMap::new();
                if offset > 0 {
                    headers.insert(header::RANGE, HeaderValue::from_str(&format!("bytes={offset}-")).map_err(NexusError::internal)?);
                }
                let (res, _) = self.s.peers.send(t, Method::GET, url, headers, Body::empty(), self.acting(), CALL_TIMEOUT).await?;
                let status = res.status();
                if !status.is_success() {
                    let bytes = read_body(res).await.unwrap_or_default();
                    return Err(peer_error(&t.name, status, &bytes));
                }
                if offset > 0 && status != StatusCode::PARTIAL_CONTENT {
                    return Err(NexusError::PeerUnreachable(format!("{} could not continue from the middle of the file", t.name)));
                }
                Ok(Body::new(res.into_body()))
            }
        }
    }

    async fn begin(&self, side: &Side, req: &NewUpload) -> Result<UploadProgress> {
        match side {
            Side::Local => uploads::begin(self.s, self.user, req).await,
            Side::Peer(t) => self.json(t, Method::POST, "/api/v1/files/uploads", Some(req), CALL_TIMEOUT).await,
        }
    }

    async fn offset(&self, side: &Side, id: &str) -> Result<u64> {
        match side {
            Side::Local => Ok(uploads::progress(self.s, self.user, id).await?.offset),
            Side::Peer(t) => {
                let p: UploadProgress =
                    self.json(t, Method::GET, &format!("/api/v1/files/uploads/{id}"), None::<&()>, CALL_TIMEOUT).await?;
                Ok(p.offset)
            }
        }
    }

    async fn append(&self, side: &Side, id: &str, offset: u64, len: u64, body: Body) -> Result<()> {
        match side {
            Side::Local => uploads::append(self.s, self.user, id, offset, body).await.map(|_| ()),
            Side::Peer(t) => {
                let mut headers = HeaderMap::new();
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
                let url = format!("/api/v1/files/uploads/{id}?offset={offset}");
                let (res, _) = self.s.peers.send(t, Method::PUT, &url, headers, body, self.acting(), TRANSFER_TIMEOUT).await?;
                let status = res.status();
                let bytes = read_body(res).await.map_err(|e| NexusError::PeerUnreachable(format!("{}: {e}", t.name)))?;
                if !status.is_success() {
                    return Err(peer_error(&t.name, status, &bytes));
                }
                Ok(())
            }
        }
    }

    async fn finish(&self, side: &Side, id: &str) -> Result<Meta> {
        match side {
            Side::Local => Ok(uploads::finish(self.s, self.user, id).await?.into()),
            Side::Peer(t) => {
                self.json(t, Method::POST, &format!("/api/v1/files/uploads/{id}/complete"), None::<&()>, TRANSFER_TIMEOUT).await
            }
        }
    }

    async fn abort(&self, side: &Side, id: &str) {
        let _ = match side {
            Side::Local => uploads::cancel(self.s, self.user, id).await,
            Side::Peer(t) => self
                .json::<serde_json::Value>(t, Method::DELETE, &format!("/api/v1/files/uploads/{id}"), None::<&()>, CALL_TIMEOUT)
                .await
                .map(|_| ()),
        };
    }

    /// Sends one file, continuing from where it stopped when the connection
    /// drops. Returns the file as it now exists at the destination.
    pub async fn send_file(&self, job: &Arc<Job>, from: &Side, src: &Meta, to: &Side, req: NewUpload) -> Result<Meta> {
        self.send_from(job, &src.filename, Source::Item(from, &src.id), to, req).await
    }

    /// Sends a file that is not in any storage location (a phone's photos),
    /// read straight from disk.
    pub async fn send_disk_file(&self, job: &Arc<Job>, path: &std::path::Path, to: &Side, req: NewUpload) -> Result<Meta> {
        let name = req.name.clone();
        self.send_from(job, &name, Source::Disk(path), to, req).await
    }

    async fn open_source(&self, src: &Source<'_>, offset: u64) -> Result<Body> {
        match src {
            Source::Item(side, id) => self.open(side, id, offset).await,
            Source::Disk(path) => {
                let mut handle = tokio::fs::File::open(path).await?;
                if offset > 0 {
                    handle.seek(std::io::SeekFrom::Start(offset)).await?;
                }
                Ok(file_body(handle))
            }
        }
    }

    /// The resumable sending loop.
    async fn send_from(&self, job: &Arc<Job>, label: &str, src: Source<'_>, to: &Side, req: NewUpload) -> Result<Meta> {
        let base = job.bytes();
        let size = req.size;
        let up = self.begin(to, &req).await?;
        let mut tries = 0;
        loop {
            if job.is_cancelled() {
                self.abort(to, &up.id).await;
                return Err(cancelled());
            }
            let attempt = async {
                let offset = if tries == 0 { 0 } else { self.offset(to, &up.id).await? };
                job.set_bytes(base + offset);
                if offset < size {
                    let body = self.open_source(&src, offset).await?;
                    self.append(to, &up.id, offset, size - offset, counted(body, job.clone())).await?;
                }
                self.finish(to, &up.id).await
            }
            .await;
            match attempt {
                Ok(placed) => {
                    job.set_bytes(base + size);
                    return Ok(placed);
                }
                Err(_) if job.is_cancelled() => {
                    self.abort(to, &up.id).await;
                    return Err(cancelled());
                }
                Err(e) if retryable(&e) && tries < RETRIES => {
                    tries += 1;
                    job.update(|i| i.retries += 1);
                    tracing::info!(file = %label, error = %e, attempt = tries, "transfer interrupted; continuing");
                    tokio::time::sleep(Duration::from_secs((1u64 << tries.min(5)).min(30))).await;
                }
                Err(e) => {
                    self.abort(to, &up.id).await;
                    job.set_bytes(base);
                    return Err(e);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Copy jobs

#[derive(Deserialize)]
pub struct CopyRequest {
    /// Device holding the items; omitted or empty for this computer.
    from_device: Option<String>,
    #[serde(default)]
    file_id: Option<String>,
    #[serde(default)]
    file_ids: Vec<String>,
    /// Device to copy to; omitted or empty for this computer.
    to_device: Option<String>,
    root_id: String,
    #[serde(default)]
    path: String,
}

enum Piece {
    File(Meta),
    Folder(Meta, Vec<TreeItem>),
}

/// `POST /api/v1/transfers/copy`: starts copying files and folders to a
/// folder on another device (or this one). Answers at once with the job.
pub async fn copy(State(s): State<SharedState>, user: AuthUser, Json(req): Json<CopyRequest>) -> ApiResult<(StatusCode, Json<JobView>)> {
    user.require_person()?;
    user.require(Action::Write)?;
    let mut ids = req.file_ids.clone();
    ids.extend(req.file_id.clone());
    ids.dedup();
    if ids.is_empty() {
        return Err(NexusError::invalid("Choose something to copy").into());
    }
    let side = |id: Option<&str>| {
        let s = s.clone();
        let id = id.filter(|d| !d.is_empty() && *d != s.node.device_id).map(str::to_string);
        async move {
            match id {
                Some(id) => Ok::<_, NexusError>(Side::Peer(peer_target(&s, &id).await?)),
                None => Ok(Side::Local),
            }
        }
    };
    let from = side(req.from_device.as_deref()).await?;
    let to = side(req.to_device.as_deref()).await?;
    let path = paths::normalize_relative(&req.path)?;

    let title = if ids.len() == 1 { "Copy".to_string() } else { format!("Copy {} items", ids.len()) };
    // The title gets the item's name once it has been looked up.
    let route = format!("{} → {}", capitalise(&from.name()), capitalise(&to.name()));
    let job = s.jobs.add("copy", title, route, user.username.clone());
    let view = job.view();
    let state = s.clone();
    tokio::spawn(async move {
        run_copy(&state, &user, &job, from, to, ids, req.root_id, path).await;
    });
    Ok((StatusCode::ACCEPTED, Json(view)))
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_copy(s: &AppState, user: &AuthUser, job: &Arc<Job>, from: Side, to: Side, ids: Vec<String>, root_id: String, path: String) {
    let Some(_slot) = s.jobs.start(s, job).await else { return };
    let mover = Mover { s, user };

    // Look at everything first, so progress has totals.
    let mut pieces = Vec::new();
    for id in &ids {
        let found = async {
            let meta = mover.info(&from, id).await?;
            if meta.is_dir {
                let items = mover.tree(&from, &meta.root_id, &meta.relative_path).await?;
                Ok::<_, NexusError>(Piece::Folder(meta, items))
            } else {
                Ok(Piece::File(meta))
            }
        }
        .await;
        match found {
            Ok(piece) => pieces.push(piece),
            Err(e) => job.fail_file(id, &e),
        }
    }
    let (files, bytes) = pieces.iter().fold((0u64, 0u64), |(f, b), p| match p {
        Piece::File(m) => (f + 1, b + m.size as u64),
        Piece::Folder(_, items) => items
            .iter()
            .filter(|i| !i.is_dir)
            .fold((f, b), |(f, b), i| (f + 1, b + i.size as u64)),
    });
    let first = pieces.first().map(|p| match p {
        Piece::File(m) | Piece::Folder(m, _) => m.filename.clone(),
    });
    job.update(|i| {
        i.files_total = files;
        i.bytes_total = bytes;
        if let (Some(name), 1) = (&first, ids.len()) {
            i.title = format!("Copy {name}");
        }
    });

    let copy_one = |src: Meta, parent: String| {
        let mover = &mover;
        let from = &from;
        let to = &to;
        let root_id = &root_id;
        async move {
            job.update(|i| i.current = Some(src.filename.clone()));
            let req = NewUpload {
                root_id: root_id.clone(),
                path: parent,
                name: src.filename.clone(),
                size: src.size as u64,
                on_conflict: None,
                modified_at: src.modified_at,
                make_parents: true,
            };
            match mover.send_file(job, from, &src, to, req).await {
                Ok(_) => job.update(|i| i.files_done += 1),
                Err(e) if job.is_cancelled() => drop(e),
                Err(e) => {
                    job.set_bytes(job.bytes() + src.size as u64);
                    job.fail_file(&src.filename, &e);
                }
            }
        }
    };

    for piece in pieces {
        if job.is_cancelled() {
            break;
        }
        match piece {
            Piece::File(meta) => copy_one(meta, path.clone()).await,
            Piece::Folder(meta, items) => {
                let top = match mover.mkdir_free(&to, &root_id, &path, &meta.filename).await {
                    Ok(name) => paths::join(&path, &name),
                    Err(e) => {
                        job.fail_file(&meta.filename, &e);
                        continue;
                    }
                };
                for item in items {
                    if job.is_cancelled() {
                        break;
                    }
                    let (parent, name) = paths::split_parent(&item.path);
                    let dest_parent = paths::join(&top, &parent);
                    if item.is_dir {
                        if let Err(e) = mover.mkdir(&to, &root_id, &dest_parent, &name).await {
                            job.fail_file(&item.path, &e);
                        }
                    } else {
                        copy_one(Meta::of_tree(&item, &meta.root_id, &meta.relative_path), dest_parent).await;
                    }
                }
            }
        }
    }

    finish_job(s, job);
    let what = match (&first, ids.len()) {
        (Some(name), 1) => name.clone(),
        _ => format!("{} items", ids.len()),
    };
    audit(s, user, "file.copied", Some(&format!("{what}: {}", job.route))).await;
}

// ---------------------------------------------------------------------------
// Job routes

pub async fn list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<JobView>>> {
    user.require_person()?;
    Ok(Json(s.jobs.views(&user)))
}

pub async fn cancel(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::Write)?;
    let job = s
        .jobs
        .find(&id)
        .filter(|j| !user.is_limited() || j.by == user.username)
        .ok_or_else(|| NexusError::NotFound("Transfer".into()))?;
    job.cancel();
    if job.info.lock().unwrap().status == "queued" {
        // It is waiting for a slot; it notices when it gets one.
        s.events.emit(Event::TransferUpdated { id: job.id.clone(), status: "cancelling".into() });
    }
    Ok(StatusCode::ACCEPTED)
}

pub async fn clear(State(s): State<SharedState>, user: AuthUser) -> ApiResult<StatusCode> {
    user.require_person()?;
    s.jobs.clear_finished(&user);
    Ok(StatusCode::NO_CONTENT)
}
