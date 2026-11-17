//! Resumable uploads.
//!
//! An upload is started with its destination and size, then its bytes are
//! sent in one or more pieces, each at the offset the server already has.
//! When a connection drops, the sender asks how much arrived and continues
//! from there. Finishing hashes the file and places it atomically.
//!
//! The same functions serve the HTTP routes (browsers, paired devices) and
//! copies and syncs this node runs itself.

use super::audit;
use super::files::{FileDetails, details};
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use futures_util::StreamExt;
use nexus_common::NexusError;
use nexus_common::time::now_ms;
use nexus_common::types::FileEntry;
use nexus_database::UploadSession;
use nexus_security::Action;
use nexus_storage::ConflictPolicy;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::{Duration, SystemTime};
use tokio::io::AsyncWriteExt;

/// Uploads not continued for this long are abandoned and cleaned up.
pub const ABANDONED_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NewUpload {
    pub root_id: String,
    #[serde(default)]
    pub path: String,
    pub name: String,
    pub size: u64,
    /// `rename` (default), `fail` or `replace` (the old content is kept as a version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_conflict: Option<String>,
    /// Modification time to give the finished file, in ms since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<i64>,
    /// Create missing folders on the way to `path`.
    #[serde(default)]
    pub make_parents: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadProgress {
    pub id: String,
    /// Bytes received so far: send the rest from here.
    pub offset: u64,
    pub size: u64,
}

/// Marks an upload as being written; dropped when the write ends.
struct Writing<'a> {
    state: &'a AppState,
    id: String,
}

impl<'a> Writing<'a> {
    fn claim(state: &'a AppState, id: &str) -> Result<Self, NexusError> {
        let mut busy = state.upload_locks.lock().unwrap();
        if !busy.insert(id.to_string()) {
            return Err(NexusError::Conflict("This upload is already being written".into()));
        }
        Ok(Self { state, id: id.to_string() })
    }
}

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        self.state.upload_locks.lock().unwrap().remove(&self.id);
    }
}

async fn owned(s: &AppState, user: &AuthUser, id: &str) -> Result<UploadSession, NexusError> {
    let session = s.db.get_upload(id).await?;
    if session.owner != user.username || !user.sees_root(&session.root_id) {
        return Err(NexusError::NotFound("Upload".into()));
    }
    Ok(session)
}

async fn received(s: &AppState, session: &UploadSession) -> Result<u64, NexusError> {
    let root = s.db.get_root(&session.root_id).await?;
    let path = s.storage.staging_path(&root, &session.id)?;
    Ok(tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0))
}

pub(crate) async fn begin(s: &AppState, user: &AuthUser, req: &NewUpload) -> Result<UploadProgress, NexusError> {
    user.require(Action::Write).map_err(|e| e.0)?;
    user.require_root(&req.root_id).map_err(|e| e.0)?;
    let policy = ConflictPolicy::parse(req.on_conflict.as_deref());
    // Validates the name, the folder and the free space before any byte is sent.
    let target = s
        .storage
        .prepare_upload_at(&req.root_id, &req.path, &req.name, Some(req.size), None, req.make_parents)
        .await?;
    let session = s
        .db
        .create_upload(
            &target.root.id,
            &target.parent,
            &target.name,
            req.size,
            policy.as_str(),
            req.modified_at,
            req.make_parents,
            &user.username,
        )
        .await?;
    Ok(UploadProgress { id: session.id, offset: 0, size: req.size })
}

pub(crate) async fn progress(s: &AppState, user: &AuthUser, id: &str) -> Result<UploadProgress, NexusError> {
    let session = owned(s, user, id).await?;
    let offset = received(s, &session).await?;
    Ok(UploadProgress { id: session.id, offset, size: session.size as u64 })
}

/// Appends `body` at `offset`, which must be exactly the number of bytes
/// already received. Whatever arrives before a connection drops is kept.
pub(crate) async fn append(s: &AppState, user: &AuthUser, id: &str, offset: u64, body: Body) -> Result<UploadProgress, NexusError> {
    user.require(Action::Write).map_err(|e| e.0)?;
    let session = owned(s, user, id).await?;
    let _writing = Writing::claim(s, id)?;
    let root = s.db.get_root(&session.root_id).await?;
    s.storage.ensure_online(&root)?;
    let path = s.storage.staging_path(&root, &session.id)?;
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut file = tokio::fs::OpenOptions::new().create(true).append(true).open(&path).await?;
    let have = file.metadata().await?.len();
    if have != offset {
        return Err(NexusError::Conflict(format!("The upload is at byte {have}, not {offset}")));
    }
    let size = session.size as u64;
    let mut written = have;
    let mut stream = body.into_data_stream();
    let mut failure = None;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                failure = Some(NexusError::PeerUnreachable(format!("Upload interrupted: {e}")));
                break;
            }
        };
        if written + chunk.len() as u64 > size {
            failure = Some(NexusError::invalid("More data was sent than the upload's size"));
            break;
        }
        if let Err(e) = file.write_all(&chunk).await {
            failure = Some(e.into());
            break;
        }
        written += chunk.len() as u64;
    }
    // Keep what arrived, even when the connection dropped: that is what
    // makes the upload resumable.
    file.flush().await?;
    file.sync_data().await?;
    drop(file);
    let _ = s.db.touch_upload(&session.id).await;
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(UploadProgress { id: session.id, offset: written, size })
}

/// Hashes the received file, places it and indexes it.
pub(crate) async fn finish(s: &AppState, user: &AuthUser, id: &str) -> Result<FileEntry, NexusError> {
    user.require(Action::Write).map_err(|e| e.0)?;
    let session = owned(s, user, id).await?;
    let _writing = Writing::claim(s, id)?;
    let target = s
        .storage
        .prepare_upload_at(
            &session.root_id,
            &session.parent_path,
            &session.filename,
            None,
            Some(&session.id),
            session.make_parents,
        )
        .await?;
    let have = tokio::fs::metadata(&target.staging).await.map(|m| m.len()).unwrap_or(0);
    if have != session.size as u64 {
        return Err(NexusError::Conflict(format!(
            "The upload is incomplete: {have} of {} bytes arrived",
            session.size
        )));
    }

    let staging = target.staging.clone();
    let modified = session.modified_at;
    let hash = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut file = std::fs::File::open(&staging)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        if let Some(ms) = modified.filter(|ms| *ms > 0) {
            let when = SystemTime::UNIX_EPOCH + Duration::from_millis(ms as u64);
            std::fs::File::options().write(true).open(&staging)?.set_modified(when)?;
        }
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .map_err(NexusError::internal)??;

    let policy = ConflictPolicy::parse(Some(&session.on_conflict));
    let rel = s.storage.commit_upload_by(&target, policy, Some(&user.username)).await?;
    s.db.delete_upload(&session.id).await?;

    s.indexer.reindex_path(&target.root, &rel).await?;
    let entry = s
        .db
        .get_file_by_path(&target.root.id, &rel)
        .await?
        .ok_or_else(|| NexusError::internal("uploaded file missing from index"))?;
    s.db.set_hash(&entry.id, Some(&hash), entry.modified_at, now_ms()).await?;
    audit(s, user, "file.uploaded", Some(&format!("{}/{}", target.root.name, rel))).await;
    s.db.get_file(&entry.id).await
}

pub(crate) async fn cancel(s: &AppState, user: &AuthUser, id: &str) -> Result<(), NexusError> {
    let session = owned(s, user, id).await?;
    let _writing = Writing::claim(s, id)?;
    if let Ok(root) = s.db.get_root(&session.root_id).await
        && let Ok(path) = s.storage.staging_path(&root, &session.id) {
            let _ = tokio::fs::remove_file(path).await;
        }
    s.db.delete_upload(&session.id).await?;
    Ok(())
}

/// Deletes uploads nobody continued for a day, with their staging files.
pub async fn clean_abandoned(s: &AppState) {
    let Ok(stale) = s.db.stale_uploads(ABANDONED_AFTER.as_millis() as i64).await else { return };
    for session in stale {
        if let Ok(root) = s.db.get_root(&session.root_id).await
            && let Ok(path) = s.storage.staging_path(&root, &session.id) {
                let _ = tokio::fs::remove_file(path).await;
            }
        let _ = s.db.delete_upload(&session.id).await;
    }
}

// -- routes ---------------------------------------------------------------

pub async fn create(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(req): Json<NewUpload>,
) -> ApiResult<(StatusCode, Json<UploadProgress>)> {
    Ok((StatusCode::CREATED, Json(begin(&s, &user, &req).await?)))
}

pub async fn status(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<UploadProgress>> {
    Ok(Json(progress(&s, &user, &id).await?))
}

#[derive(Deserialize)]
pub struct AppendQuery {
    offset: u64,
}

pub async fn put(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Query(q): Query<AppendQuery>,
    body: Body,
) -> ApiResult<Json<UploadProgress>> {
    Ok(Json(append(&s, &user, &id, q.offset, body).await?))
}

pub async fn complete(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<FileDetails>)> {
    let entry = finish(&s, &user, &id).await?;
    Ok((StatusCode::CREATED, Json(details(&s, entry).await?)))
}

pub async fn remove(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    cancel(&s, &user, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
