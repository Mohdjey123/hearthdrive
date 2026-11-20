use super::audit;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::StatusCode;
use nexus_common::NexusError;
use nexus_common::types::{ScanState, StorageRoot};
use nexus_database::RootStats;
use nexus_security::Action;
use nexus_storage::drives::{self, DiskSpace, DriveInfo};
use nexus_storage::{AddRootRequest, StorageManager};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Serialize)]
pub struct RootView {
    #[serde(flatten)]
    root: StorageRoot,
    online: bool,
    scanning: bool,
    stats: RootStats,
    scan: Option<ScanState>,
    disk: Option<DiskSpace>,
}

pub(crate) async fn root_view(s: &AppState, root: StorageRoot, drives: &[DriveInfo]) -> ApiResult<RootView> {
    let online = StorageManager::is_online(&root);
    let stats = s.db.root_stats(&root.id).await?;
    let scan = s.db.get_scan_state(&root.id).await?;
    let path = root.absolute_path.to_lowercase();
    let disk = online
        .then(|| {
            drives
                .iter()
                .filter(|d| path.starts_with(&d.mount_point.to_lowercase()))
                .max_by_key(|d| d.mount_point.len())
                .map(|d| DiskSpace { total_bytes: d.total_bytes, available_bytes: d.available_bytes })
        })
        .flatten();
    let scanning = s.indexer.is_scanning(&root.id);
    Ok(RootView { root, online, scanning, stats, scan, disk })
}

async fn list_drives() -> Vec<DriveInfo> {
    tokio::task::spawn_blocking(drives::list_drives).await.unwrap_or_default()
}

/// The computer's drives, for adding storage: only for people who may add it.
pub async fn drives(user: AuthUser) -> ApiResult<Json<Vec<DriveInfo>>> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    Ok(Json(list_drives().await))
}

pub async fn list_roots(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<RootView>>> {
    user.require(Action::Read)?;
    let drives = list_drives().await;
    let mut out = Vec::new();
    for root in s.db.list_roots().await? {
        if user.sees_root(&root.id) {
            out.push(root_view(&s, root, &drives).await?);
        }
    }
    Ok(Json(out))
}

pub async fn root_stats(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<RootView>> {
    user.require(Action::Read)?;
    user.require_root(&id)?;
    let root = s.db.get_root(&id).await?;
    Ok(Json(root_view(&s, root, &list_drives().await).await?))
}

#[derive(Deserialize)]
pub struct AddRootBody {
    path: String,
    name: Option<String>,
    #[serde(default)]
    read_only: bool,
}

/// Registers a folder or drive. Absolute filesystem paths are only accepted
/// from this computer, never from remote devices.
pub async fn add_root(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<AddRootBody>,
) -> ApiResult<(StatusCode, Json<RootView>)> {
    user.require(Action::ManageStorage)?;
    user.require_person()?;
    if !peer.ip().is_loopback() {
        return Err(NexusError::PermissionDenied("Storage can only be added on the computer itself".into()).into());
    }
    let root = s
        .storage
        .add_root(AddRootRequest { path: body.path, name: body.name, read_only: body.read_only })
        .await?;
    s.indexer.register_root(&root);
    audit(&s, &user, "storage.root.added", Some(&root.absolute_path)).await;
    let view = root_view(&s, root, &list_drives().await).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Setting: "1" while the phone's own storage is hidden from paired devices.
const PHONE_PRIVATE_SETTING: &str = "phone.storage.private";
const PHONE_ROOT_NAME: &str = "Phone storage";

/// The phone's own storage as a storage location (phones only): whether it
/// is added, and whether paired computers may browse it.
#[derive(Serialize)]
pub struct PhoneStorage {
    available: bool,
    path: Option<String>,
    root_id: Option<String>,
    private: bool,
}

/// The storage location that is the phone's whole shared storage, if added.
async fn phone_root(s: &AppState) -> Option<StorageRoot> {
    let media = s.media_dir.as_ref()?;
    let want = media.to_string_lossy().trim_end_matches('/').to_string();
    let roots = s.db.list_roots().await.ok()?;
    roots.into_iter().find(|r| r.absolute_path.trim_end_matches('/') == want)
}

async fn phone_private(s: &AppState) -> bool {
    s.db.get_setting(PHONE_PRIVATE_SETTING).await.ok().flatten().as_deref() == Some("1")
}

/// Storage locations paired devices may not see: the phone's own storage
/// while it is kept to the phone.
pub(crate) async fn hidden_from_devices(s: &AppState) -> Vec<String> {
    if s.media_dir.is_none() || !phone_private(s).await {
        return Vec::new();
    }
    phone_root(s).await.map(|r| vec![r.id]).unwrap_or_default()
}

async fn phone_view(s: &AppState) -> PhoneStorage {
    PhoneStorage {
        available: s.media_dir.is_some(),
        path: s.media_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
        root_id: phone_root(s).await.map(|r| r.id),
        private: phone_private(s).await,
    }
}

pub async fn phone_storage(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<PhoneStorage>> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    Ok(Json(phone_view(&s).await))
}

#[derive(Deserialize)]
pub struct PhoneStorageBody {
    enabled: Option<bool>,
    private: Option<bool>,
}

/// Adds or removes the phone's own storage, and keeps it from paired
/// devices or not. Only on the phone itself.
pub async fn set_phone_storage(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<PhoneStorageBody>,
) -> ApiResult<Json<PhoneStorage>> {
    user.require(Action::ManageStorage)?;
    user.require_person()?;
    if !peer.ip().is_loopback() {
        return Err(NexusError::PermissionDenied("This can only be changed on the phone itself".into()).into());
    }
    let media = s.media_dir.clone().ok_or_else(|| NexusError::Unavailable("Only phones have phone storage".into()))?;
    if let Some(private) = body.private {
        s.db.set_setting(PHONE_PRIVATE_SETTING, if private { "1" } else { "0" }).await?;
        audit(&s, &user, if private { "storage.phone.private" } else { "storage.phone.shared" }, None).await;
    }
    match (body.enabled, phone_root(&s).await) {
        (Some(true), None) => {
            let request = AddRootRequest {
                path: media.to_string_lossy().into_owned(),
                name: Some(PHONE_ROOT_NAME.into()),
                read_only: false,
            };
            let root = s.storage.add_root(request).await?;
            s.indexer.register_root(&root);
            audit(&s, &user, "storage.root.added", Some(&root.absolute_path)).await;
            s.events.emit(nexus_common::events::Event::StorageChanged { root_id: root.id });
        }
        (Some(false), Some(root)) => {
            s.indexer.unregister_root(&root.id);
            s.storage.remove_root(&root.id).await?;
            audit(&s, &user, "storage.root.removed", Some(&root.absolute_path)).await;
            s.events.emit(nexus_common::events::Event::StorageChanged { root_id: root.id });
        }
        _ => {}
    }
    Ok(Json(phone_view(&s).await))
}

pub async fn remove_root(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    user.require(Action::ManageStorage)?;
    user.require_person()?;
    s.indexer.unregister_root(&id);
    let root = s.storage.remove_root(&id).await?;
    audit(&s, &user, "storage.root.removed", Some(&root.absolute_path)).await;
    s.events.emit(nexus_common::events::Event::StorageChanged { root_id: id });
    Ok(StatusCode::NO_CONTENT)
}

pub async fn rescan_root(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    user.require(Action::ManageStorage)?;
    user.require_person()?;
    user.require_root(&id)?;
    let root = s.db.get_root(&id).await?;
    s.storage.ensure_online(&root)?;
    if s.indexer.is_scanning(&id) {
        return Err(NexusError::Conflict("A scan is already running".into()).into());
    }
    s.indexer.spawn_scan(root);
    Ok(StatusCode::ACCEPTED)
}
