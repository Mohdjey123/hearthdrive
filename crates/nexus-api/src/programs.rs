//! The address for programs: scripts using the API, WebDAV clients and S3
//! tools, all signed in with tokens for programs. It is off until turned
//! on, listens on this computer only unless other computers on the network
//! are allowed, and takes nothing but those tokens.
//!
//! - `/api/v1/…`: the API, as the app uses it.
//! - `/dav/{storage}/…`: WebDAV.
//! - everything else: the S3-compatible API (path-style buckets).

use crate::auth::AuthUser;
use crate::state::{AppState, SharedState};
use crate::{s3, webdav};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{self, any, post};
use nexus_common::types::StorageRoot;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;

const ENABLED: &str = "programs.enabled";
const PORT: &str = "programs.port";
const LAN: &str = "programs.lan";
pub const DEFAULT_PORT: u16 = 47180;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgramSettings {
    pub enabled: bool,
    pub port: u16,
    /// Other computers on the network may connect (otherwise only this one).
    pub lan: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ProgramStatus {
    pub running: bool,
    pub error: Option<String>,
    /// Addresses programs can use.
    pub addresses: Vec<String>,
}

#[derive(Default)]
pub struct Programs {
    running: Mutex<Option<(tokio::task::JoinHandle<()>, ProgramStatus)>>,
}

impl Programs {
    pub fn status(&self) -> ProgramStatus {
        self.running.lock().unwrap().as_ref().map(|r| r.1.clone()).unwrap_or_default()
    }

    fn stop(&self) {
        if let Some((task, _)) = self.running.lock().unwrap().take() {
            task.abort();
        }
    }
}

pub async fn settings(s: &AppState) -> ProgramSettings {
    let get = |k: &'static str| async move { s.db.get_setting(k).await.ok().flatten() };
    ProgramSettings {
        enabled: get(ENABLED).await.as_deref() == Some("true"),
        port: get(PORT).await.and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT),
        lan: get(LAN).await.as_deref() == Some("true"),
    }
}

pub async fn save(s: &AppState, v: &ProgramSettings) -> nexus_common::Result<()> {
    s.db.set_setting(ENABLED, if v.enabled { "true" } else { "false" }).await?;
    s.db.set_setting(PORT, &v.port.to_string()).await?;
    s.db.set_setting(LAN, if v.lan { "true" } else { "false" }).await?;
    Ok(())
}

/// Starts (or restarts, or stops) the listener as the settings say.
pub async fn apply(state: &SharedState) -> ProgramStatus {
    state.programs.stop();
    let v = settings(state).await;
    if !v.enabled || std::env::var_os("NEXUS_NO_PROGRAMS").is_some() {
        return ProgramStatus::default();
    }
    let ip = if v.lan { IpAddr::V4(Ipv4Addr::UNSPECIFIED) } else { IpAddr::V4(Ipv4Addr::LOCALHOST) };
    let listener = match tokio::net::TcpListener::bind(SocketAddr::new(ip, v.port)).await {
        Ok(l) => l,
        Err(e) => {
            let status = ProgramStatus {
                running: false,
                error: Some(format!("Port {} could not be used: {e}", v.port)),
                addresses: Vec::new(),
            };
            tracing::warn!(port = v.port, error = %e, "could not start the address for programs");
            *state.programs.running.lock().unwrap() = Some((tokio::spawn(async {}), status.clone()));
            return status;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(v.port);
    let mut addresses = vec![format!("http://127.0.0.1:{port}")];
    if v.lan {
        for ip in local_ips() {
            addresses.push(format!("http://{ip}:{port}"));
        }
    }
    let app = router(state.clone());
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
            tracing::warn!(error = %e, "the address for programs stopped");
        }
    });
    tracing::info!(port, lan = v.lan, "address for programs is listening");
    let status = ProgramStatus { running: true, error: None, addresses };
    *state.programs.running.lock().unwrap() = Some((task, status.clone()));
    status
}

fn local_ips() -> Vec<IpAddr> {
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| i.is_up() && !i.is_loopback())
        .flat_map(|i| i.ipv4.into_iter().map(|n| IpAddr::V4(n.addr())))
        .filter(|ip| match ip {
            IpAddr::V4(v4) => v4.is_private(),
            _ => false,
        })
        .collect()
}

pub fn spawn(state: SharedState) {
    tokio::spawn(async move {
        apply(&state).await;
    });
}

/// What the address for programs serves (public for tests).
pub fn router(state: SharedState) -> Router {
    Router::<SharedState>::new()
        .route("/dav", any(webdav::handle))
        .route("/dav/", any(webdav::handle))
        .route("/dav/{*rest}", any(webdav::handle))
        .route("/upload/{id}/{secret}", routing::get(crate::upload_links::upload_page))
        .route("/upload/{id}/{secret}/files", post(crate::upload_links::upload_file))
        .fallback(s3::handle)
        .with_state(state.clone())
        .nest("/api/v1", crate::api_routes(state))
        .layer(middleware::from_fn(tokens_only))
}

/// Only tokens for programs here: no sessions, no sign-in, no first-run
/// setup, and no links meant for browsers.
async fn tokens_only(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api/v1") {
        let token = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);
        if !token.is_some_and(|t| t.starts_with(crate::tokens::PREFIX)) {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({ "error": {
                    "code": "unauthorized",
                    "message": "Use a token for programs: Authorization: Bearer nxt_…",
                } })),
            )
                .into_response();
        }
    }
    next.run(req).await
}

/// Names for storage locations in addresses (S3 buckets, WebDAV folders):
/// lower case letters, digits and dashes, unique.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    let mut out = if out.len() < 3 { format!("storage-{out}") } else { out };
    out.truncate(55);
    out.trim_end_matches('-').to_string()
}

/// This computer's storage the caller may see, each with its name in
/// addresses.
pub async fn named_roots(s: &AppState, user: &AuthUser) -> Vec<(String, StorageRoot)> {
    let mut roots: Vec<StorageRoot> = s
        .db
        .list_roots()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.device_id == s.node.device_id && r.enabled)
        .collect();
    roots.sort_by_key(|r| r.created_at);
    let mut taken: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for root in roots {
        let base = slug(&root.name);
        let mut name = base.clone();
        let mut n = 2;
        while taken.contains(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        taken.push(name.clone());
        if user.sees_any(&root.id) {
            out.push((name, root));
        }
    }
    out
}

/// `GET /programs`
pub async fn get(State(s): State<SharedState>, user: AuthUser) -> crate::error::ApiResult<axum::Json<serde_json::Value>> {
    user.require_person()?;
    let v = settings(&s).await;
    Ok(axum::Json(serde_json::json!({ "settings": v, "status": s.programs.status() })))
}

/// `PUT /programs`: owners and administrators.
pub async fn put(
    State(s): State<SharedState>,
    user: AuthUser,
    axum::Json(v): axum::Json<ProgramSettings>,
) -> crate::error::ApiResult<axum::Json<serde_json::Value>> {
    user.require_person()?;
    if !matches!(user.role, nexus_common::types::Role::Owner | nexus_common::types::Role::Administrator) {
        return Err(nexus_common::NexusError::PermissionDenied("Only an owner or administrator can change this".into()).into());
    }
    if v.port < 1024 {
        return Err(nexus_common::NexusError::invalid("Choose a port from 1024 to 65535").into());
    }
    save(&s, &v).await?;
    let status = apply(&s).await;
    crate::routes::audit(&s, &user, "programs.changed", Some(&format!("on={} port={} network={}", v.enabled, v.port, v.lan))).await;
    Ok(axum::Json(serde_json::json!({ "settings": v, "status": status })))
}

#[cfg(test)]
mod tests {
    #[test]
    fn storage_names_fit_in_addresses() {
        assert_eq!(super::slug("My Photos (2024)"), "my-photos-2024");
        assert_eq!(super::slug("D:"), "storage-d");
        assert_eq!(super::slug("Übung 1"), "bung-1");
    }
}

#[derive(Serialize)]
pub struct TokenView {
    #[serde(flatten)]
    token: nexus_database::ApiToken,
    username: Option<String>,
}

/// `GET /tokens`: your tokens (owners and administrators see everyone's).
pub async fn list_tokens(State(s): State<SharedState>, user: AuthUser) -> crate::error::ApiResult<axum::Json<Vec<TokenView>>> {
    user.require_person()?;
    let me = user.user_id.clone().ok_or(nexus_common::NexusError::PermissionDenied("Sign in as a person".into()))?;
    let everyone = matches!(user.role, nexus_common::types::Role::Owner | nexus_common::types::Role::Administrator);
    let tokens = s.db.list_api_tokens(if everyone { None } else { Some(&me) }).await?;
    let users = s.db.list_users().await.unwrap_or_default();
    Ok(axum::Json(
        tokens
            .into_iter()
            .map(|t| {
                let username = users.iter().find(|u| u.id == t.user_id).map(|u| u.username.clone());
                TokenView { token: t, username }
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct NewToken {
    name: String,
    #[serde(default)]
    read_only: bool,
    /// Days until it stops working; none: until removed.
    expires_days: Option<u32>,
}

#[derive(Serialize)]
pub struct CreatedToken {
    #[serde(flatten)]
    token: nexus_database::ApiToken,
    /// Shown once: `Authorization: Bearer …`, or the WebDAV password.
    secret_token: String,
    /// For S3 tools.
    access_key: String,
    secret_key: String,
}

/// `POST /tokens`
pub async fn create_token(
    State(s): State<SharedState>,
    user: AuthUser,
    axum::Json(body): axum::Json<NewToken>,
) -> crate::error::ApiResult<(StatusCode, axum::Json<CreatedToken>)> {
    user.require_person()?;
    let me = user.user_id.clone().ok_or(nexus_common::NexusError::PermissionDenied("Sign in as a person".into()))?;
    let name = body.name.trim();
    if name.is_empty() || name.len() > 80 {
        return Err(nexus_common::NexusError::invalid("Give the token a name (what will use it)").into());
    }
    let expires = body.expires_days.filter(|d| *d > 0).map(|d| nexus_common::time::now_ms() + d as i64 * 24 * 3600 * 1000);
    let (token, full) = crate::tokens::generate(&s, &me, name, body.read_only, expires);
    s.db.create_api_token(&token).await?;
    crate::routes::audit(&s, &user, "token.created", Some(name)).await;
    let (id, secret) = crate::tokens::split(&full).map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
    Ok((StatusCode::CREATED, axum::Json(CreatedToken { token, secret_token: full, access_key: id, secret_key: secret })))
}

/// `DELETE /tokens/{id}`
pub async fn delete_token(
    State(s): State<SharedState>,
    user: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> crate::error::ApiResult<StatusCode> {
    user.require_person()?;
    let token = s.db.get_api_token(&id).await?;
    let everyone = matches!(user.role, nexus_common::types::Role::Owner | nexus_common::types::Role::Administrator);
    if !everyone && user.user_id.as_deref() != Some(token.user_id.as_str()) {
        return Err(nexus_common::NexusError::PermissionDenied("This is someone else's token".into()).into());
    }
    s.db.delete_api_token(&id).await?;
    crate::routes::audit(&s, &user, "token.removed", Some(&token.name)).await;
    Ok(StatusCode::NO_CONTENT)
}
