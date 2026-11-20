//! The `/api/v1` HTTP and WebSocket API.
//!
//! REST for file operations, a WebSocket for live events. Every route except
//! health and first-run setup requires authentication, and authorisation is
//! checked here on the backend, never only in the UI.

mod auth;
pub mod automation;
mod error;
pub mod docmeta;
pub mod documents;
pub mod media;
pub mod nearby;
pub mod net;
pub mod p2p;
pub mod peers;
pub mod plugins;
pub mod power;
pub mod programs;
pub mod s3;
pub mod webdav;
pub mod winrt;
mod routes;
pub mod share;

pub use routes::backup::spawn as spawn_backup;
pub use routes::changes::spawn as spawn_changes;
pub use routes::introductions::{introduce_now as introduce_devices, spawn as spawn_introductions};
pub use routes::health::spawn as spawn_health;
pub use routes::shares::spawn as spawn_shares;
pub use routes::sync::spawn as spawn_sync;
pub use routes::snapshots::spawn as spawn_snapshots;
pub use routes::transfers::spawn as spawn_transfers;
pub use routes::updates::{check as check_updates, spawn as spawn_updates};
pub use routes::vdrive::{saved as saved_vdrive, saved_cache as saved_vdrive_cache};

/// The signed update packages a node keeps and passes on.
pub mod update_store {
    pub use crate::routes::updates::{AUTO_INSTALL_SETTING, import, package_path, stored};
}
mod state;
pub mod tokens;
pub mod upload_links;
pub mod vault;

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

/// The API's routes, under `/api/v1`.
pub(crate) fn api_routes(state: SharedState) -> Router {
    use routes::{backup, commands, devices, library, photos, files, health, introductions, network, security, shares, sharing, snapshots, storage, sync, system, tools, transfers, updates, uploads, users, vdrive};
    Router::new()
        .route("/health", get(system::health))
        .route("/setup/status", get(system::setup_status))
        .route("/setup", post(system::setup))
        .route("/auth/login", post(system::login))
        .route("/auth/login/code", post(security::login_code))
        .route("/auth/passkey/start", post(security::passkey_start))
        .route("/auth/passkey", post(security::passkey_sign_in))
        .route("/auth/security", get(security::get))
        .route("/auth/security/log", get(security::log))
        .route("/auth/2fa/setup", post(security::setup_two_factor))
        .route("/auth/2fa/enable", post(security::enable_two_factor))
        .route("/auth/2fa/disable", post(security::disable_two_factor))
        .route("/auth/2fa/recovery-codes", post(security::new_recovery_codes))
        .route("/auth/passkeys/start", post(security::add_passkey_start))
        .route("/auth/passkeys", post(security::add_passkey))
        .route("/auth/passkeys/{id}", delete(security::remove_passkey))
        .route("/auth/sessions/{id}", delete(security::end_session))
        .route("/auth/sessions/end-others", post(security::end_other_sessions))
        .route("/auth/logout", post(system::logout))
        .route("/auth/me", get(system::me))
        .route("/system/info", get(system::info))
        .route("/activity", get(system::activity))
        // Devices and pairing
        .route("/devices", get(devices::list))
        .route("/devices/{id}", patch(devices::update).delete(devices::remove))
        .route("/devices/introductions", get(introductions::settings).put(introductions::set_settings))
        .route("/devices/pair", post(devices::create_invite))
        .route("/devices/nearby", get(devices::nearby))
        .route("/library/music", get(library::music))
        .route("/library/items", get(library::items))
        .route("/library/capabilities", get(library::capabilities))
        .route("/playback", get(library::continue_watching))
        .route("/playback/{id}", get(library::get_playback).put(library::set_playback))
        .route("/tags", get(library::tags))
        .route("/documents/settings", axum::routing::put(library::set_document_settings))
        .route("/documents", get(routes::documents::list))
        .route("/documents/{id}", get(routes::documents::get).patch(routes::documents::update))
        .route("/photos", get(photos::list))
        .route("/photos/months", get(photos::months))
        .route("/photos/places", get(photos::places))
        .route("/photos/memories", get(photos::memories))
        .route("/photos/similar", get(photos::similar))
        .route("/albums", get(photos::albums).post(photos::create_album))
        .route("/albums/{id}", get(photos::album).patch(photos::change_album).delete(photos::delete_album))
        .route("/commands", get(commands::list))
        .route("/plugins", get(plugins::get_all))
        .route("/plugins/commands", get(plugins::commands))
        .route("/plugins/{id}", axum::routing::put(plugins::set_enabled))
        .route("/plugins/{id}/commands/{command}", post(plugins::run))
        .route("/files/{id}/encryption", get(vault::state))
        .route("/files/{id}/encryption/lock", post(vault::lock))
        .route("/files/{id}/encryption/unlock", post(vault::unlock))
        .route("/programs", get(programs::get).put(programs::put))
        .route("/tokens", get(programs::list_tokens).post(programs::create_token))
        .route("/tokens/{id}", delete(programs::delete_token))
        .route("/rules", get(routes::rules::list).post(routes::rules::create))
        .route("/rules/preview", post(routes::rules::preview_draft))
        .route("/rules/{id}", axum::routing::put(routes::rules::update).delete(routes::rules::remove))
        .route("/rules/{id}/run", post(routes::rules::run))
        .route("/rules/{id}/preview", get(routes::rules::preview))
        .route("/storage/suggestions", get(routes::rules::suggestions))
        .route("/commands/{name}", post(commands::run))
        .route("/devices/{id}/wake", post(devices::wake))
        .route("/power/wake", post(devices::wake_for_peer))
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
        .route("/snapshots", get(snapshots::list).post(snapshots::create))
        .route("/snapshots/open", post(snapshots::open))
        .route("/snapshots/{id}", patch(snapshots::update).delete(snapshots::remove))
        .route("/snapshots/{id}/run", post(snapshots::run))
        .route("/snapshots/{id}/check", post(snapshots::check))
        .route("/snapshots/{id}/recovery-key", post(snapshots::recovery_key))
        .route("/snapshots/{id}/history", get(snapshots::history))
        .route("/snapshots/{id}/{snapshot}/files", get(snapshots::files))
        .route("/snapshots/{id}/{snapshot}/restore", post(snapshots::start_restore))
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
        .route("/sync/{id}/check", post(sync::check))
        .route("/sync/{id}/held", get(sync::held))
        .route("/sync/{id}/held/restore", post(sync::bring_back))
        .route("/backup", get(backup::get).put(backup::set).patch(backup::update).delete(backup::remove))
        .route("/backup/run", post(backup::run))
        .route("/shared", get(backup::shared))
        .route("/shared/send", post(backup::send_shared))
        .route("/shared/discard", post(backup::discard_shared))
        .route("/shares", get(shares::list).post(shares::create))
        .route("/shares/services", get(shares::services).put(shares::set_services))
        .route("/shares/refresh", post(shares::refresh))
        .route("/shares/{id}", delete(shares::remove))
        .route("/shared-folders", get(sharing::list).post(sharing::create))
        .route("/shared-folders/mine", get(sharing::mine))
        .route("/shared-folders/{id}", patch(sharing::update).delete(sharing::remove))
        .route("/groups", get(sharing::groups).post(sharing::create_group))
        .route("/upload-links", get(crate::upload_links::list).post(crate::upload_links::create))
        .route("/upload-links/{id}", delete(crate::upload_links::remove))
        .route("/groups/{id}", patch(sharing::update_group).delete(sharing::delete_group))
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", patch(users::update).delete(users::remove))
        .route("/updates", get(updates::status))
        .route("/updates/check", post(updates::check_now))
        .route("/updates/settings", axum::routing::put(updates::set_settings))
        .route("/updates/install", post(updates::install))
        .route("/system/autostart", get(system::autostart).put(system::set_autostart))
        .route("/vdrive", get(vdrive::get).put(vdrive::set))
        .route("/vdrive/cache", axum::routing::put(vdrive::set_cache))
        .route("/network", get(network::get).put(network::update))
        .route("/network/refresh", post(network::refresh))
        .route("/events", get(routes::events::events))
        .route("/storage/drives", get(storage::drives))
        .route("/storage/changes", get(routes::changes::changes))
        .route("/storage/roots", get(storage::list_roots).post(storage::add_root))
        .route("/storage/roots/{id}", delete(storage::remove_root))
        .route("/storage/roots/{id}/stats", get(storage::root_stats))
        .route("/storage/roots/{id}/scan", post(storage::rescan_root))
        .route("/storage/roots/{id}/verify", post(health::verify))
        .route("/storage/health", get(health::get))
        .route("/storage/health/refresh", post(health::refresh))
        .route("/storage/health/damaged/{file_id}/repair", post(health::repair_damaged))
        .route("/storage/health/damaged/{file_id}/accept", post(health::accept_damaged))
        .route("/files", get(files::list))
        .route("/files/folder", post(files::create_folder))
        .route(
            "/files/upload",
            post(files::upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/files/tree", get(files::tree))
        .route("/files/by-hash/{hash}", get(tools::by_hash))
        .route("/files/duplicates", get(tools::duplicates))
        .route("/files/fetch", post(tools::fetch))
        .route("/files/{id}/archive", get(tools::archive))
        .route("/files/{id}/extract", post(tools::extract))
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
        .route("/files/{id}/thumbnail", get(photos::thumbnail))
        .route("/files/{id}/stream", get(library::stream))
        .route("/files/{id}/stream-link", post(library::stream_link))
        .route("/files/{id}/tags", get(library::file_tags).put(library::set_file_tags))
        .route("/files/{id}/restore", post(files::restore))
        .route("/files/{id}/comments", get(sharing::comments).post(sharing::add_comment))
        .route("/files/{id}/comments/{comment}", delete(sharing::remove_comment))
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
                    "message": "This HearthDrive service does not support this yet. It may be an older version: restart HearthDrive, or update the device it runs on.",
                } })),
            )
        })
        .with_state(state)
}

pub fn router(state: SharedState, ui_dir: Option<PathBuf>) -> Router {
    let api = api_routes(state.clone());

    let mut app = Router::new().nest("/api/v1", api);
    if let Some(dir) = ui_dir {
        let index = dir.join("index.html");
        let ui = Router::new()
            .fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)))
            .layer(middleware::map_response_with_state((), ui_caching));
        app = app.fallback_service(ui);
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

/// The web interface: the page itself is checked every time, so an update
/// shows at once; its scripts and styles carry their content's hash in their
/// names, so they are kept.
async fn ui_caching(State(()): State<()>, uri: axum::http::Uri, mut res: Response) -> Response {
    if res.status().is_success() {
        let value = if uri.path().starts_with("/assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    }
    res
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
