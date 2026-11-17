use super::audit;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::net::{NetSnapshot, REMOTE_ENABLED_SETTING};
use crate::state::SharedState;
use axum::Json;
use axum::extract::State;
use nexus_security::Action;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize)]
pub struct NetworkView {
    #[serde(flatten)]
    snapshot: NetSnapshot,
    device_id: String,
    fingerprint: String,
}

fn view(s: &SharedState) -> NetworkView {
    NetworkView {
        snapshot: s.net.snapshot(),
        device_id: s.node.device_id.clone(),
        fingerprint: s.identity.fingerprint().to_string(),
    }
}

pub async fn get(State(s): State<SharedState>, _user: AuthUser) -> ApiResult<Json<NetworkView>> {
    Ok(Json(view(&s)))
}

/// Re-checks the network now and contacts paired devices.
pub async fn refresh(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<NetworkView>> {
    user.require_person()?;
    let generation = s.net.snapshot().generation;
    s.net.wake.notify_one();
    s.net.wait_for_update(generation, Duration::from_secs(8)).await;
    s.net.peer_wake.notify_one();
    Ok(Json(view(&s)))
}

#[derive(Deserialize)]
pub struct NetworkSettings {
    remote_enabled: bool,
}

/// Turns remote access on or off. Off refuses and closes incoming
/// peer-to-peer connections; this computer can still reach its peers.
pub async fn update(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(req): Json<NetworkSettings>,
) -> ApiResult<Json<NetworkView>> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    s.db.set_setting(REMOTE_ENABLED_SETTING, if req.remote_enabled { "1" } else { "0" }).await?;
    audit(&s, &user, if req.remote_enabled { "network.remote.enabled" } else { "network.remote.disabled" }, None).await;
    let generation = s.net.snapshot().generation;
    s.net.wake.notify_one();
    s.net.wait_for_update(generation, Duration::from_secs(8)).await;
    Ok(Json(view(&s)))
}
