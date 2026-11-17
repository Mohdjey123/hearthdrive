//! The `/api/v1` HTTP and WebSocket API.
//!
//! REST for file operations, a WebSocket for live events. Every route except
//! health and first-run setup requires authentication, and authorisation is
//! checked here on the backend, never only in the UI.

mod auth;
mod error;
pub mod net;
pub mod p2p;
pub mod peers;
mod routes;
mod state;

pub use state::{AppState, NodeInfo, SharedState};

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, patch, post};
use std::path::PathBuf;
use std::time::Duration;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

/// Origins of the desktop shell's webview and the Vite dev server.
const ALLOWED_ORIGINS: &[&str] = &[
    "http://tauri.localhost",
    "https://tauri.localhost",
    "tauri://localhost",
    "http://localhost:1420",
    "http://127.0.0.1:1420",
];

pub fn router(state: SharedState, ui_dir: Option<PathBuf>) -> Router {
    use routes::{devices, files, network, storage, system};

    let api = Router::new()
        .route("/health", get(system::health))
        .route("/setup/status", get(system::setup_status))
        .route("/setup", post(system::setup))
        .route("/auth/login", post(system::login))
        .route("/auth/logout", post(system::logout))
        .route("/auth/me", get(system::me))
        .route("/system/info", get(system::info))
        .route("/activity", get(system::activity))
        // Devices and pairing
        .route("/devices", get(devices::list))
        .route("/devices/{id}", patch(devices::update).delete(devices::remove))
        .route("/devices/pair", post(devices::create_invite))
        .route("/devices/pair/{id}", delete(devices::cancel_invite))
        .route("/devices/pair/requests", get(devices::list_requests))
        .route("/devices/pair/requests/{id}/approve", post(devices::approve))
        .route("/devices/pair/requests/{id}/reject", post(devices::reject))
        .route("/pairing/request", post(devices::incoming_request))
        .route("/pairing/requests/{id}/status", get(devices::request_status))
        .route("/pairing/join", post(devices::join))
        .route("/pairing/join/{id}", get(devices::join_status))
        // Called by paired devices (signed requests)
        .route("/peer/hello", get(devices::hello))
        .route("/peer/unpair", post(devices::unpair))
        // Browse a paired device through this node
        .route(
            "/peers/{device_id}/proxy/{*rest}",
            any(peers::proxy).layer(DefaultBodyLimit::disable()),
        )
        .route("/network", get(network::get).put(network::update))
        .route("/network/refresh", post(network::refresh))
        .route("/events", get(routes::events::events))
        .route("/storage/drives", get(storage::drives))
        .route("/storage/roots", get(storage::list_roots).post(storage::add_root))
        .route("/storage/roots/{id}", delete(storage::remove_root))
        .route("/storage/roots/{id}/stats", get(storage::root_stats))
        .route("/storage/roots/{id}/scan", post(storage::rescan_root))
        .route("/files", get(files::list))
        .route("/files/folder", post(files::create_folder))
        .route(
            "/files/upload",
            post(files::upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/files/{id}", get(files::get).patch(files::update).delete(files::remove))
        .route("/files/{id}/link", post(files::link))
        .route("/files/{id}/content", get(files::content))
        .route("/files/{id}/restore", post(files::restore))
        .route("/trash", get(files::trash))
        .route("/trash/{id}", delete(files::purge))
        .route("/search", get(files::search))
        .route("/recent", get(files::recent))
        .with_state(state.clone());

    let mut app = Router::new().nest("/api/v1", api);
    if let Some(dir) = ui_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(
            ALLOWED_ORIGINS.iter().map(|o| HeaderValue::from_static(o)),
        ))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH, Method::DELETE])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::RANGE])
        .expose_headers([header::CONTENT_DISPOSITION, header::CONTENT_LENGTH])
        .max_age(Duration::from_secs(600));

    app.layer(cors)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(middleware::from_fn_with_state(state, host_guard))
        .layer(TraceLayer::new_for_http())
}

/// Rejects requests whose Host header does not name this node on loopback,
/// which defeats DNS rebinding attacks from web pages against the API.
/// Requests that arrived over the peer-to-peer network are identified by
/// their connection instead.
async fn host_guard(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let allowed = req.extensions().get::<p2p::P2pPeer>().is_some()
        || req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| state.net.host_allowed(h));
    if !allowed {
        return (StatusCode::MISDIRECTED_REQUEST, "Host not allowed").into_response();
    }
    next.run(req).await
}
