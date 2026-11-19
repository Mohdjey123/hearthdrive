//! The virtual drive setting: paired devices in File Explorer.

use super::audit;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{ConnectInfo, State};
use nexus_common::NexusError;
use nexus_security::Action;
use nexus_vdrive::{CACHE_SETTING, CacheSettings, ENABLED_SETTING, PATH_SETTING, Status, check_path};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;

/// The saved choice: on or off, and the folder (`None`: the default).
pub async fn saved(s: &AppState) -> nexus_common::Result<(bool, Option<PathBuf>)> {
    let enabled = s.db.get_setting(ENABLED_SETTING).await?.is_some_and(|v| v == "1");
    let mut path = s.db.get_setting(PATH_SETTING).await?.filter(|p| !p.trim().is_empty()).map(PathBuf::from);
    if enabled && path.is_none() {
        // Turned on in the default folder before the app was renamed (the
        // folder is saved since): it stays in that folder.
        let earlier = nexus_vdrive::earlier_default_path();
        s.db.set_setting(PATH_SETTING, &earlier.display().to_string()).await?;
        path = Some(earlier);
    }
    Ok((enabled, path))
}

/// The saved offline cache settings.
pub async fn saved_cache(s: &AppState) -> CacheSettings {
    s.db.get_setting(CACHE_SETTING)
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

/// `PUT /vdrive/cache`: the offline cache's size limit and clearing.
pub async fn set_cache(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(body): Json<CacheSettings>,
) -> ApiResult<Json<Status>> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    let settings = body.validate().map_err(NexusError::invalid)?;
    s.db.set_setting(CACHE_SETTING, &serde_json::to_string(&settings).map_err(|e| NexusError::internal(e.to_string()))?).await?;
    s.vdrive.set_cache(settings);
    audit(&s, &user, "vdrive.cache", None).await;
    Ok(Json(s.vdrive.status()))
}

/// `GET /vdrive`: whether the drive is on, where, and whether it runs.
pub async fn get(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Status>> {
    user.require_person()?;
    Ok(Json(s.vdrive.status()))
}

#[derive(Deserialize)]
pub struct SetBody {
    enabled: bool,
    /// The folder; empty or missing for the default.
    path: Option<String>,
}

/// `PUT /vdrive`: turns the drive on (in a folder) or off. Only on the
/// computer itself, by someone who may manage storage.
pub async fn set(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<SetBody>,
) -> ApiResult<Json<Status>> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    if !peer.ip().is_loopback() {
        return Err(NexusError::PermissionDenied("This can only be changed on the computer itself".into()).into());
    }
    let mut path = body.path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).map(PathBuf::from);
    if !body.enabled && path.is_none() {
        // Turning off keeps the chosen folder for next time.
        path = saved(&s).await?.1;
    }
    if body.enabled {
        // The folder is always saved, so a later change of the default
        // never moves a drive.
        let chosen = path.get_or_insert_with(nexus_vdrive::default_path).clone();
        let mut avoid: Vec<PathBuf> = s
            .db
            .list_roots()
            .await?
            .into_iter()
            .filter(|r| r.device_id == s.node.device_id)
            .map(|r| PathBuf::from(r.absolute_path))
            .collect();
        avoid.push(s.data_dir.clone());
        check_path(&chosen, &avoid).map_err(NexusError::invalid)?;
    }
    let status = s.vdrive.apply(body.enabled, path.clone()).await.map_err(NexusError::invalid)?;
    s.db.set_setting(ENABLED_SETTING, if body.enabled { "1" } else { "0" }).await?;
    s.db.set_setting(PATH_SETTING, &path.map(|p| p.display().to_string()).unwrap_or_default()).await?;
    audit(&s, &user, if body.enabled { "vdrive.enabled" } else { "vdrive.disabled" }, Some(&status.path)).await;
    Ok(Json(status))
}
