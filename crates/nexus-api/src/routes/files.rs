use super::{audit, clamp_limit};
use crate::auth::{AuthUser, authenticate, bearer};
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use futures_util::StreamExt;
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::types::{Availability, DeletedFile, FileEntry, StorageRoot};
use nexus_common::NexusError;
use nexus_database::SortField;
use nexus_security::Action;
use nexus_storage::{ConflictPolicy, StorageManager, paths};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;

/// Content tickets outlive a long video so range requests keep working.
const CONTENT_TICKET_TTL_SECS: i64 = 6 * 60 * 60;

#[derive(Serialize)]
pub struct RootSummary {
    id: String,
    name: String,
    online: bool,
    read_only: bool,
}

impl From<&StorageRoot> for RootSummary {
    fn from(r: &StorageRoot) -> Self {
        Self { id: r.id.clone(), name: r.name.clone(), online: StorageManager::is_online(r), read_only: r.read_only }
    }
}

/// Reports offline availability for entries on disconnected roots.
fn overlay(mut entry: FileEntry, online: bool) -> FileEntry {
    if !online {
        entry.availability = Availability::Offline;
    }
    entry
}

#[derive(Deserialize)]
pub struct ListQuery {
    root_id: String,
    path: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    offset: Option<i64>,
    limit: Option<i64>,
}

#[derive(Serialize)]
pub struct ListResponse {
    root: RootSummary,
    path: String,
    items: Vec<FileEntry>,
    total: i64,
    offset: i64,
    limit: i64,
}

pub async fn list(
    State(s): State<SharedState>,
    user: AuthUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<ListResponse>> {
    user.require(Action::Read)?;
    user.require_root(&q.root_id)?;
    let root = s.db.get_root(&q.root_id).await?;
    let path = paths::normalize_relative(q.path.as_deref().unwrap_or(""))?;
    if !path.is_empty() {
        match s.db.get_file_by_path(&root.id, &path).await? {
            Some(dir) if dir.is_dir => {}
            _ => return Err(NexusError::NotFound("Folder".into()).into()),
        }
    }
    let offset = q.offset.unwrap_or(0).max(0);
    let limit = clamp_limit(q.limit, 200, 1000);
    let (items, total) = s
        .db
        .list_children(
            &root.id,
            &path,
            SortField::parse(q.sort.as_deref()),
            q.order.as_deref() == Some("desc"),
            offset,
            limit,
        )
        .await?;
    let online = StorageManager::is_online(&root);
    Ok(Json(ListResponse {
        root: RootSummary::from(&root),
        path,
        items: items.into_iter().map(|e| overlay(e, online)).collect(),
        total,
        offset,
        limit,
    }))
}

#[derive(Serialize)]
pub(crate) struct FileDetails {
    #[serde(flatten)]
    file: FileEntry,
    root: RootSummary,
}

pub(crate) async fn details(s: &AppState, file: FileEntry) -> ApiResult<FileDetails> {
    let root = s.db.get_root(&file.root_id).await?;
    let online = StorageManager::is_online(&root);
    Ok(FileDetails { file: overlay(file, online), root: RootSummary::from(&root) })
}

pub async fn get(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<FileDetails>> {
    user.require(Action::Read)?;
    let file = s.db.get_file(&id).await?;
    user.require_root(&file.root_id)?;
    Ok(Json(details(&s, file).await?))
}

#[derive(Deserialize)]
pub struct LinkQuery {
    download: Option<u8>,
}

#[derive(Serialize)]
pub struct LinkResponse {
    url: String,
    expires_in: i64,
}

/// Issues a short-lived signed URL for previews, streaming and downloads.
pub async fn link(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Query(q): Query<LinkQuery>,
) -> ApiResult<Json<LinkResponse>> {
    user.require(Action::Read)?;
    let file = s.db.get_file(&id).await?;
    user.require_root(&file.root_id)?;
    if file.is_dir {
        return Err(NexusError::invalid("Folders cannot be downloaded yet").into());
    }
    let ticket = s.tickets.issue("content", &file.id, CONTENT_TICKET_TTL_SECS);
    let download = if q.download == Some(1) { "&download=1" } else { "" };
    Ok(Json(LinkResponse {
        url: format!("/api/v1/files/{}/content?ticket={ticket}{download}", file.id),
        expires_in: CONTENT_TICKET_TTL_SECS,
    }))
}

#[derive(Deserialize)]
pub struct ContentQuery {
    ticket: Option<String>,
    download: Option<u8>,
}

pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn content_disposition(download: bool, filename: &str) -> HeaderValue {
    let kind = if download { "attachment" } else { "inline" };
    let fallback: String = filename
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || " .-_()[]".contains(c) { c } else { '_' })
        .collect();
    HeaderValue::from_str(&format!(
        "{kind}; filename=\"{fallback}\"; filename*=UTF-8''{}",
        percent_encode(filename)
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// Streams file content with HTTP range support (video seeking, resumable
/// downloads). Authorised by a content ticket or a bearer token.
pub async fn content(
    State(s): State<SharedState>,
    Path(id): Path<String>,
    Query(q): Query<ContentQuery>,
    req: Request,
) -> ApiResult<Response> {
    // A ticket was issued after checking the caller's access to this file.
    // A session is checked here, like any other request.
    let caller = match q.ticket.as_deref() {
        Some(ticket) if s.tickets.verify(ticket, "content", &id) => None,
        Some(_) => return Err(NexusError::Unauthorized.into()),
        None => match bearer(req.headers()) {
            Some(token) => Some(authenticate(&s, token).await?),
            None => return Err(NexusError::Unauthorized.into()),
        },
    };
    let file = s.db.get_file(&id).await?;
    if let Some(user) = &caller {
        user.require(Action::Read)?;
        user.require_root(&file.root_id)?;
    }
    if file.is_dir {
        return Err(NexusError::invalid("This is a folder").into());
    }
    let root = s.db.get_root(&file.root_id).await?;
    s.storage.ensure_online(&root)?;
    let path = s.storage.resolve(&root, &file.relative_path)?;

    let response = match ServeFile::new(&path).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let mut response = response.map(Body::new);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_DISPOSITION, content_disposition(q.download == Some(1), &file.filename));
    // Opened directly, user content runs in an opaque origin: an uploaded
    // HTML or SVG file can never read the NEXUS session.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=300"));
    Ok(response)
}

#[derive(Deserialize)]
pub struct UploadQuery {
    root_id: String,
    path: Option<String>,
    name: String,
    on_conflict: Option<String>,
}

async fn write_body(path: &std::path::Path, body: Body, declared: Option<u64>) -> Result<String, NexusError> {
    let mut file = tokio::fs::File::create(path).await?;
    let mut hasher = blake3::Hasher::new();
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| NexusError::invalid(format!("Upload interrupted: {e}")))?;
        written += chunk.len() as u64;
        if declared.is_some_and(|d| written > d) {
            return Err(NexusError::invalid("Upload is larger than declared"));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    if declared.is_some_and(|d| written != d) {
        return Err(NexusError::invalid("Upload ended early"));
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Streams the raw request body into a staging file on the destination
/// volume, hashing as it goes, then places it atomically without ever
/// overwriting an existing file.
pub async fn upload(
    State(s): State<SharedState>,
    user: AuthUser,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<(StatusCode, Json<FileDetails>)> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let policy = ConflictPolicy::parse(q.on_conflict.as_deref());
    let entry = store_stream(&s, &user, &q.root_id, q.path.as_deref().unwrap_or(""), &q.name, declared, body, policy).await?;
    Ok((StatusCode::CREATED, Json(details(&s, entry).await?)))
}

/// Stores a stream as a new file in a local storage location: the shared
/// path of uploads and of copies arriving from another device.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn store_stream(
    s: &AppState,
    user: &AuthUser,
    root_id: &str,
    parent: &str,
    name: &str,
    declared: Option<u64>,
    body: Body,
    policy: ConflictPolicy,
) -> ApiResult<FileEntry> {
    user.require(Action::Write)?;
    user.require_root(root_id)?;
    let target = s.storage.prepare_upload(root_id, parent, name, declared).await?;

    let placed = async {
        let hash = write_body(&target.staging, body, declared).await?;
        let rel = s.storage.commit_upload_by(&target, policy, Some(&user.username)).await?;
        Ok::<_, NexusError>((hash, rel))
    }
    .await;
    let (hash, rel) = match placed {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_file(&target.staging).await;
            return Err(e.into());
        }
    };

    s.indexer.reindex_path(&target.root, &rel).await?;
    let entry = s
        .db
        .get_file_by_path(&target.root.id, &rel)
        .await?
        .ok_or_else(|| NexusError::internal("uploaded file missing from index"))?;
    s.db.set_hash(&entry.id, Some(&hash), entry.modified_at, now_ms()).await?;
    audit(s, user, "file.uploaded", Some(&format!("{}/{}", target.root.name, rel))).await;
    Ok(s.db.get_file(&entry.id).await?)
}

#[derive(Deserialize)]
pub struct TreeQuery {
    root_id: String,
    #[serde(default)]
    path: String,
}

/// One entry of a folder tree, with its path relative to the folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeItem {
    pub id: String,
    pub path: String,
    pub is_dir: bool,
    pub size: i64,
    pub modified_at: Option<i64>,
    pub content_hash: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct TreeResponse {
    pub items: Vec<TreeItem>,
}

/// Everything under `base` in a root, paths relative to `base`, parents first.
pub(crate) async fn tree_of(s: &AppState, root_id: &str, base: &str) -> Result<Vec<TreeItem>, NexusError> {
    let base = paths::normalize_relative(base)?;
    if !base.is_empty() {
        match s.db.get_file_by_path(root_id, &base).await? {
            Some(dir) if dir.is_dir => {}
            _ => return Err(NexusError::NotFound("Folder".into())),
        }
    }
    let skip = if base.is_empty() { 0 } else { base.len() + 1 };
    Ok(s
        .db
        .subtree_entries(root_id, &base)
        .await?
        .into_iter()
        .filter(|e| !paths::is_hidden_internal(&e.relative_path))
        .map(|e| TreeItem {
            path: e.relative_path.get(skip..).unwrap_or_default().to_string(),
            id: e.id,
            is_dir: e.is_dir,
            size: e.size,
            modified_at: e.modified_at,
            content_hash: e.content_hash,
        })
        .collect())
}

/// `GET /files/tree`: every file and folder under a folder, for copying
/// folders and comparing them during sync.
pub async fn tree(State(s): State<SharedState>, user: AuthUser, Query(q): Query<TreeQuery>) -> ApiResult<Json<TreeResponse>> {
    user.require(Action::Read)?;
    user.require_root(&q.root_id)?;
    let root = s.db.get_root(&q.root_id).await?;
    // An incomplete listing would look like deleted files to a sync.
    s.storage.ensure_online(&root)?;
    if s.indexer.is_scanning(&root.id) {
        return Err(NexusError::Unavailable(format!("{} is still being indexed; try again shortly", root.name)).into());
    }
    Ok(Json(TreeResponse { items: tree_of(&s, &q.root_id, &q.path).await? }))
}

// -- versions ---------------------------------------------------------------

pub async fn versions(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<nexus_database::FileVersion>>> {
    user.require(Action::Read)?;
    let file = s.db.get_file(&id).await?;
    user.require_root(&file.root_id)?;
    Ok(Json(s.db.list_versions(&file.id).await?))
}

pub async fn restore_version(
    State(s): State<SharedState>,
    user: AuthUser,
    Path((id, version)): Path<(String, String)>,
) -> ApiResult<Json<FileDetails>> {
    user.require(Action::Write)?;
    let file = s.db.get_file(&id).await?;
    user.require_root(&file.root_id)?;
    if file.is_dir {
        return Err(NexusError::invalid("Folders have no versions").into());
    }
    let root = s.storage.restore_version(&file, &version, Some(&user.username)).await?;
    s.indexer.reindex_path(&root, &file.relative_path).await?;
    s.events.emit(Event::FileModified { root_id: root.id.clone(), path: file.relative_path.clone() });
    audit(&s, &user, "file.version.restored", Some(&format!("{}/{}", root.name, file.relative_path))).await;
    Ok(Json(details(&s, s.db.get_file(&file.id).await?).await?))
}

pub async fn delete_version(
    State(s): State<SharedState>,
    user: AuthUser,
    Path((id, version)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    user.require(Action::Write)?;
    let file = s.db.get_file(&id).await?;
    user.require_root(&file.root_id)?;
    s.storage.delete_version(&file, &version).await?;
    audit(&s, &user, "file.version.deleted", Some(&file.relative_path)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A short-lived URL to download an earlier version.
pub async fn version_link(
    State(s): State<SharedState>,
    user: AuthUser,
    Path((id, version)): Path<(String, String)>,
) -> ApiResult<Json<LinkResponse>> {
    user.require(Action::Read)?;
    let file = s.db.get_file(&id).await?;
    user.require_root(&file.root_id)?;
    let v = s.db.get_version(&version).await?;
    if v.file_id != file.id {
        return Err(NexusError::NotFound("Version".into()).into());
    }
    let ticket = s.tickets.issue("version", &v.id, CONTENT_TICKET_TTL_SECS);
    Ok(Json(LinkResponse {
        url: format!("/api/v1/files/{}/versions/{}/content?ticket={ticket}", file.id, v.id),
        expires_in: CONTENT_TICKET_TTL_SECS,
    }))
}

pub async fn version_content(
    State(s): State<SharedState>,
    Path((id, version)): Path<(String, String)>,
    Query(q): Query<ContentQuery>,
    req: Request,
) -> ApiResult<Response> {
    if !q.ticket.as_deref().is_some_and(|t| s.tickets.verify(t, "version", &version)) {
        return Err(NexusError::Unauthorized.into());
    }
    let file = s.db.get_file(&id).await?;
    let v = s.db.get_version(&version).await?;
    if v.file_id != file.id {
        return Err(NexusError::NotFound("Version".into()).into());
    }
    let root = s.db.get_root(&v.root_id).await?;
    s.storage.ensure_online(&root)?;
    let path = s.storage.version_path(&root, &v.id);
    let response = match ServeFile::new(&path).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let mut response = response.map(Body::new);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_DISPOSITION, content_disposition(true, &file.filename));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'"));
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    Ok(response)
}

#[derive(Deserialize)]
pub struct FolderBody {
    root_id: String,
    #[serde(default)]
    path: String,
    name: String,
}

pub async fn create_folder(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(body): Json<FolderBody>,
) -> ApiResult<(StatusCode, Json<FileDetails>)> {
    user.require(Action::Write)?;
    user.require_root(&body.root_id)?;
    let (root, rel) = s.storage.create_folder(&body.root_id, &body.path, &body.name).await?;
    s.indexer.reindex_path(&root, &rel).await?;
    let entry = s
        .db
        .get_file_by_path(&root.id, &rel)
        .await?
        .ok_or_else(|| NexusError::internal("new folder missing from index"))?;
    audit(&s, &user, "folder.created", Some(&format!("{}/{}", root.name, rel))).await;
    Ok((StatusCode::CREATED, Json(details(&s, entry).await?)))
}

#[derive(Deserialize)]
pub struct UpdateBody {
    name: Option<String>,
    parent_path: Option<String>,
}

/// Rename and/or move within the same storage root. The file keeps its id.
pub async fn update(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<FileDetails>> {
    user.require(Action::Write)?;
    if body.name.is_none() && body.parent_path.is_none() {
        return Err(NexusError::invalid("Nothing to change").into());
    }
    let file = s.db.get_file(&id).await?;
    user.require_root(&file.root_id)?;
    let new_rel = s
        .storage
        .rename_or_move(&file, body.parent_path.as_deref(), body.name.as_deref())
        .await?;
    if new_rel != file.relative_path {
        s.events.emit(Event::FileMoved {
            root_id: file.root_id.clone(),
            from: file.relative_path.clone(),
            to: new_rel.clone(),
        });
        audit(&s, &user, "file.moved", Some(&format!("{} → {}", file.relative_path, new_rel))).await;
    }
    let updated = s.db.get_file(&id).await?;
    Ok(Json(details(&s, updated).await?))
}

/// Moves a file or folder to the root's trash (recoverable).
pub async fn remove(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<DeletedFile>> {
    user.require(Action::Write)?;
    let file = s.db.get_file(&id).await?;
    user.require_root(&file.root_id)?;
    let record = s.storage.trash(&file, user.user_id.as_deref()).await?;
    s.events.emit(Event::FileDeleted { root_id: file.root_id.clone(), path: file.relative_path.clone() });
    audit(&s, &user, "file.trashed", Some(&file.relative_path)).await;
    Ok(Json(record))
}

pub async fn restore(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<FileDetails>> {
    user.require(Action::Write)?;
    user.require_root(&s.db.get_deleted(&id).await?.root_id)?;
    let (root, rel) = s.storage.restore(&id).await?;
    s.indexer.reindex_path(&root, &rel).await?;
    // Give the restored entry back its original id.
    let _ = s.db.set_id_for_path(&root.id, &rel, &id).await;
    let entry = match s.db.get_file(&id).await {
        Ok(e) => e,
        Err(_) => s
            .db
            .get_file_by_path(&root.id, &rel)
            .await?
            .ok_or_else(|| NexusError::internal("restored item missing from index"))?,
    };
    audit(&s, &user, "file.restored", Some(&rel)).await;
    Ok(Json(details(&s, entry).await?))
}

#[derive(Serialize)]
pub struct TrashItem {
    #[serde(flatten)]
    item: DeletedFile,
    root_name: String,
}

pub async fn trash(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<TrashItem>>> {
    user.require(Action::Read)?;
    let names = root_names(&s).await?;
    Ok(Json(
        s.db.list_deleted()
            .await?
            .into_iter()
            .filter(|item| user.sees_root(&item.root_id))
            .map(|item| TrashItem { root_name: names.get(&item.root_id).cloned().unwrap_or_default(), item })
            .collect(),
    ))
}

/// Permanently deletes a trashed item. Cannot be undone.
pub async fn purge(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require(Action::ManageStorage)?;
    user.require_root(&s.db.get_deleted(&id).await?.root_id)?;
    let record = s.storage.purge(&id).await?;
    audit(&s, &user, "file.purged", Some(&record.original_path)).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn root_names(s: &SharedState) -> Result<HashMap<String, String>, ApiError> {
    Ok(s.db.list_roots().await?.into_iter().map(|r| (r.id, r.name)).collect())
}

#[derive(Serialize)]
pub struct FoundItem {
    #[serde(flatten)]
    file: FileEntry,
    root_name: String,
}

async fn with_roots(s: &SharedState, files: Vec<FileEntry>) -> ApiResult<Vec<FoundItem>> {
    let roots: HashMap<String, StorageRoot> =
        s.db.list_roots().await?.into_iter().map(|r| (r.id.clone(), r)).collect();
    Ok(files
        .into_iter()
        .map(|f| {
            let (name, online) = roots
                .get(&f.root_id)
                .map(|r| (r.name.clone(), StorageManager::is_online(r)))
                .unwrap_or_default();
            FoundItem { file: overlay(f, online), root_name: name }
        })
        .collect())
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
    root_id: Option<String>,
    offset: Option<i64>,
    limit: Option<i64>,
}

#[derive(Serialize)]
pub struct SearchResponse {
    items: Vec<FoundItem>,
    has_more: bool,
}

pub async fn search(
    State(s): State<SharedState>,
    user: AuthUser,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<SearchResponse>> {
    user.require(Action::Read)?;
    if q.q.trim().is_empty() {
        return Ok(Json(SearchResponse { items: vec![], has_more: false }));
    }
    let limit = clamp_limit(q.limit, 100, 500);
    let mut found = s
        .db
        .search(&q.q, q.root_id.as_deref(), q.offset.unwrap_or(0).max(0), limit + 1)
        .await?;
    let has_more = found.len() as i64 > limit;
    found.truncate(limit as usize);
    found.retain(|f| user.sees_root(&f.root_id));
    Ok(Json(SearchResponse { items: with_roots(&s, found).await?, has_more }))
}

#[derive(Deserialize)]
pub struct RecentQuery {
    limit: Option<i64>,
}

pub async fn recent(
    State(s): State<SharedState>,
    user: AuthUser,
    Query(q): Query<RecentQuery>,
) -> ApiResult<Json<Vec<FoundItem>>> {
    user.require(Action::Read)?;
    let mut files = s.db.recent_files(clamp_limit(q.limit, 20, 200)).await?;
    files.retain(|f| user.sees_root(&f.root_id));
    Ok(Json(with_roots(&s, files).await?))
}
