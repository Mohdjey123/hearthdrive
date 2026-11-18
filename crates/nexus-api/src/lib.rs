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
pub mod share;

pub use routes::backup::spawn as spawn_backup;
pub use routes::introductions::{introduce_now as introduce_devices, spawn as spawn_introductions};
pub use routes::shares::spawn as spawn_shares;
pub use routes::sync::spawn as spawn_sync;
pub use routes::transfers::spawn as spawn_transfers;
pub use routes::updates::{check as check_updates, spawn as spawn_updates};

/// The signed update packages a node keeps and passes on.
pub mod update_store {
    pub use crate::routes::updates::{AUTO_INSTALL_SETTING, import, package_path, stored};
}
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

/// Origins of the app shell's web view.
const ALLOWED_ORIGINS: &[&str] = &["http://tauri.localhost", "https://tauri.localhost", "tauri://localhost"];

/// The Vite dev server, allowed in development builds only: in a release
/// build any program could serve a page there.
const DEV_ORIGINS: &[&str] = &["http://localhost:1420", "http://127.0.0.1:1420"];

/// Origins allowed to call the API from a browser.
pub(crate) fn allowed_origins() -> impl Iterator<Item = &'static str> {
    let dev: &[&str] = if cfg!(debug_assertions) { DEV_ORIGINS } else { &[] };
    ALLOWED_ORIGINS.iter().chain(dev).copied()
}

/// The web interface served to browsers loads nothing from anywhere else and
/// can never be framed.
const UI_CSP: &str = "default-src 'self'; connect-src 'self' ws://127.0.0.1:* ws://localhost:*; \
    img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self' 'unsafe-inline'; font-src 'self' data:; \
    frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

pub fn router(state: SharedState, ui_dir: Option<PathBuf>) -> Router {
    use routes::{backup, devices, files, introductions, network, shares, storage, sync, system, transfers, updates, uploads, users};

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
        .route("/devices/introductions", get(introductions::settings).put(introductions::set_settings))
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
        .route("/peer/introductions", post(introductions::receive))
        .route("/peer/updates", get(updates::peer_list))
        .route("/peer/updates/{platform}", get(updates::peer_package))
        // Browse a paired device through this node
        .route(
            "/peers/{device_id}/proxy/{*rest}",
            any(peers::proxy).layer(DefaultBodyLimit::disable()),
        )
        .route("/transfers", get(transfers::list))
        .route("/transfers/copy", post(transfers::copy))
        .route("/transfers/clear", post(transfers::clear))
        .route("/transfers/{id}/cancel", post(transfers::cancel))
        .route("/transfers/{id}/pause", post(transfers::pause))
        .route("/transfers/{id}/resume", post(transfers::resume))
        .route("/transfers/{id}/first", post(transfers::first))
        .route("/transfers/{id}/retry", post(transfers::retry))
        .route("/transfers/settings", get(transfers::settings).put(transfers::set_settings))
        .route("/sync", get(sync::list).post(sync::create))
        .route("/sync/{id}", patch(sync::update).delete(sync::remove))
        .route("/sync/{id}/run", post(sync::run))
        .route("/backup", get(backup::get).put(backup::set).patch(backup::update).delete(backup::remove))
        .route("/backup/run", post(backup::run))
        .route("/shared", get(backup::shared))
        .route("/shared/send", post(backup::send_shared))
        .route("/shared/discard", post(backup::discard_shared))
        .route("/shares", get(shares::list).post(shares::create))
        .route("/shares/services", get(shares::services).put(shares::set_services))
        .route("/shares/refresh", post(shares::refresh))
        .route("/shares/{id}", delete(shares::remove))
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", patch(users::update).delete(users::remove))
        .route("/updates", get(updates::status))
        .route("/updates/check", post(updates::check_now))
        .route("/updates/settings", axum::routing::put(updates::set_settings))
        .route("/updates/install", post(updates::install))
        .route("/system/autostart", get(system::autostart).put(system::set_autostart))
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
        .route("/files/tree", get(files::tree))
        .route("/files/uploads", post(uploads::create))
        .route(
            "/files/uploads/{id}",
            get(uploads::status).put(uploads::put).delete(uploads::remove).layer(DefaultBodyLimit::disable()),
        )
        .route("/files/uploads/{id}/complete", post(uploads::complete))
        .route("/files/{id}", get(files::get).patch(files::update).delete(files::remove))
        .route("/files/{id}/versions", get(files::versions))
        .route("/files/{id}/versions/{version}", delete(files::delete_version))
        .route("/files/{id}/versions/{version}/restore", post(files::restore_version))
        .route("/files/{id}/versions/{version}/link", post(files::version_link))
        .route("/files/{id}/versions/{version}/content", get(files::version_content))
        .route("/files/{id}/link", post(files::link))
        .route("/files/{id}/content", get(files::content))
        .route("/files/{id}/restore", post(files::restore))
        .route("/trash", get(files::trash))
        .route("/trash/{id}", delete(files::purge))
        .route("/search", get(files::search))
        .route("/recent", get(files::recent))
        // An address this service does not know: most likely an app newer
        // than the service (or a paired device running an older version).
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({ "error": {
                    "code": "unknown_route",
                    "message": "This NEXUS service does not support this yet. It may be an older version: restart NEXUS, or update the device it runs on.",
                } })),
            )
        })
        .with_state(state.clone());

    let mut app = Router::new().nest("/api/v1", api);
    if let Some(dir) = ui_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins().map(HeaderValue::from_static)))
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
        // File content and proxied responses set their own, stricter policy.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(UI_CSP),
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
