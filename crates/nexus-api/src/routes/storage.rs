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
