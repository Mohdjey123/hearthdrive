use super::{audit, clamp_limit};
use crate::auth::{AuthUser, bearer};
use crate::error::ApiResult;
use crate::state::SharedState;
use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use nexus_common::time::now_ms;
use nexus_common::types::{ActivityEntry, User};
use nexus_common::{NexusError, VERSION};
use nexus_security::{generate_token, hash_password, hash_token, verify_password};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Duration;

const SESSION_DAYS: i64 = 30;

pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}

pub async fn setup_status(State(s): State<SharedState>) -> ApiResult<Json<Value>> {
    let needs_setup = s.db.count_users().await? == 0;
    let device = s.db.get_device(&s.node.device_id).await?;
    Ok(Json(json!({ "needs_setup": needs_setup, "node_name": device.name })))
}

#[derive(Deserialize)]
pub struct SetupRequest {
    node_name: String,
    username: String,
    password: String,
}

#[derive(Serialize)]
pub struct SessionResponse {
    token: String,
    expires_at: i64,
    user: User,
}

fn validate_username(username: &str) -> Result<(), NexusError> {
    let ok = (3..=32).contains(&username.len())
        && username.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(NexusError::invalid("Usernames are 3–32 letters, digits, '.', '_' or '-'"))
    }
}

async fn issue_session(s: &SharedState, user: User, client: Option<&str>) -> ApiResult<SessionResponse> {
    let token = generate_token();
    let expires_at = now_ms() + SESSION_DAYS * 24 * 60 * 60 * 1000;
    s.db.create_session(&user.id, &hash_token(&token), expires_at, client).await?;
    Ok(SessionResponse { token, expires_at, user })
}

/// First-run setup: names this node and creates the owner account.
/// Only accepted from this computer, and only while no account exists.
pub async fn setup(
    State(s): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SetupRequest>,
) -> ApiResult<Json<SessionResponse>> {
    if !peer.ip().is_loopback() {
        return Err(NexusError::PermissionDenied("Setup must be completed on this computer".into()).into());
    }
    let node_name = req.node_name.trim();
    if node_name.is_empty() || node_name.len() > 64 {
        return Err(NexusError::invalid("Give this computer a name (up to 64 characters)").into());
    }
    let username = req.username.trim();
    validate_username(username)?;
    if req.password.chars().count() < 8 {
        return Err(NexusError::invalid("Use a password of at least 8 characters").into());
    }
    let password = req.password.clone();
    let hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(NexusError::internal)??;
    let user = s
        .db
        .create_first_owner(username, &hash)
        .await?
        .ok_or_else(|| NexusError::Conflict("This node is already set up".into()))?;
    s.db.rename_device(&s.node.device_id, node_name).await?;

    let caller = crate::auth::AuthUser {
        user_id: Some(user.id.clone()),
        username: user.username.clone(),
        role: user.role,
        local: true,
        device_id: None,
        allowed_roots: None,
    };
    audit(&s, &caller, "setup.completed", Some(node_name)).await;
    let client = headers.get("user-agent").and_then(|v| v.to_str().ok());
    Ok(Json(issue_session(&s, user, client).await?))
}

#[derive(Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

pub async fn login(
    State(s): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Json<SessionResponse>> {
    s.throttle.check(peer.ip())?;
    let found = s.db.find_credentials(req.username.trim()).await?;
    let password = req.password.clone();
    let candidate = found.clone();
    let valid = tokio::task::spawn_blocking(move || match &candidate {
        Some((_, hash)) => verify_password(&password, hash),
        None => {
            // Spend comparable time for unknown users.
            let _ = hash_password(&password);
            false
        }
    })
    .await
    .map_err(NexusError::internal)?;

    let Some((user, _)) = found.filter(|_| valid) else {
        s.throttle.fail(peer.ip());
        tokio::time::sleep(Duration::from_millis(400)).await;
        return Err(NexusError::Unauthorized.into());
    };
    let caller = crate::auth::AuthUser {
        user_id: Some(user.id.clone()),
        username: user.username.clone(),
        role: user.role,
        local: false,
        device_id: None,
        allowed_roots: None,
    };
    s.throttle.clear(peer.ip());
    audit(&s, &caller, "auth.login", None).await;
    let client = headers.get("user-agent").and_then(|v| v.to_str().ok());
    Ok(Json(issue_session(&s, user, client).await?))
}

pub async fn logout(State(s): State<SharedState>, user: AuthUser, headers: HeaderMap) -> ApiResult<StatusCode> {
    if !user.local
        && let Some(token) = bearer(&headers) {
            s.db.delete_session(&hash_token(token)).await?;
        }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn me(user: AuthUser) -> Json<AuthUser> {
    Json(user)
}

pub async fn info(State(s): State<SharedState>, _user: AuthUser) -> ApiResult<Json<Value>> {
    let device = s.db.get_device(&s.node.device_id).await?;
    Ok(Json(json!({
        "node": {
            "device_id": device.id,
            "name": device.name,
            "hostname": s.node.hostname,
            "version": s.node.version,
            "platform": std::env::consts::OS,
        },
        "started_at": s.started_at,
        "uptime_ms": now_ms() - s.started_at,
        "data_dir": s.data_dir.to_string_lossy(),
        "network": s.net.snapshot(),
        "fingerprint": s.identity.fingerprint(),
    })))
}

#[derive(Deserialize)]
pub struct LimitQuery {
    limit: Option<i64>,
}

pub async fn activity(
    State(s): State<SharedState>,
    user: AuthUser,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Vec<ActivityEntry>>> {
    let mut entries = s.db.recent_activity(clamp_limit(q.limit, 50, 500)).await?;
    // The log names files everywhere; people limited to some storage only
    // see what they did themselves.
    if user.is_limited() {
        entries.retain(|e| e.username.as_deref() == Some(user.username.as_str()));
    }
    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// Start with Windows

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "NEXUS Cloud";

#[cfg(windows)]
fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output()
}

/// Whether this service is registered to start when the user signs in.
#[cfg(windows)]
fn autostart_enabled() -> bool {
    reg(&["query", RUN_KEY, "/v", RUN_VALUE]).map(|o| o.status.success()).unwrap_or(false)
}

#[cfg(windows)]
fn write_autostart(enabled: bool) -> Result<(), NexusError> {
    let output = if enabled {
        let exe = std::env::current_exe()?;
        let command = format!("\"{}\"", exe.display());
        reg(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f"])?
    } else if autostart_enabled() {
        reg(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])?
    } else {
        return Ok(());
    };
    if output.status.success() {
        Ok(())
    } else {
        Err(NexusError::internal(format!("Windows refused the change: {}", String::from_utf8_lossy(&output.stderr).trim())))
    }
}

#[cfg(not(windows))]
fn autostart_enabled() -> bool {
    false
}

#[cfg(not(windows))]
fn write_autostart(_enabled: bool) -> Result<(), NexusError> {
    Err(NexusError::invalid("Starting automatically is only available on Windows"))
}

/// `GET /system/autostart`: whether the service starts when you sign in.
pub async fn autostart(user: AuthUser) -> ApiResult<Json<Value>> {
    user.require_person()?;
    let enabled = tokio::task::spawn_blocking(autostart_enabled).await.unwrap_or(false);
    Ok(Json(json!({ "supported": cfg!(windows), "enabled": enabled })))
}

#[derive(Deserialize)]
pub struct AutostartBody {
    enabled: bool,
}

/// `PUT /system/autostart`: registers or removes the per-user start-up entry
/// (no administrator rights needed; it only affects this Windows account).
pub async fn set_autostart(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<AutostartBody>,
) -> ApiResult<Json<Value>> {
    user.require_person()?;
    user.require(nexus_security::Action::ManageStorage)?;
    if !peer.ip().is_loopback() {
        return Err(NexusError::PermissionDenied("This can only be changed on the computer itself".into()).into());
    }
    let enabled = body.enabled;
    tokio::task::spawn_blocking(move || write_autostart(enabled)).await.map_err(NexusError::internal)??;
    audit(&s, &user, if enabled { "system.autostart.enabled" } else { "system.autostart.disabled" }, None).await;
    Ok(Json(json!({ "supported": cfg!(windows), "enabled": enabled })))
}
