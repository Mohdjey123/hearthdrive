//! HTTP over the peer-to-peer endpoint.
//!
//! Each request travels on its own QUIC bidirectional stream as plain
//! HTTP/1.1. The connection's TLS handshake authenticates the remote node's
//! key, which is recorded on the request as [`P2pPeer`]: signed requests must
//! come from the key that signed them, and pairing requests from the key they
//! present. Only the parts of the API that devices use are served here.

use crate::error::ApiError;
use crate::state::SharedState;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::response::{IntoResponse, Response};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use nexus_common::NexusError;
use nexus_devices::signing::HDR_DEVICE;
use nexus_network::{Connection, key_string};
use std::convert::Infallible;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use tower::ServiceExt;

/// The authenticated key (base64url) of the node a request arrived from.
#[derive(Debug, Clone)]
pub struct P2pPeer(pub String);

/// API areas reachable over the peer-to-peer network. Everything else
/// (sign-in, setup, device management, the event stream, relaying to other
/// nodes) is for people on this computer only. Photos, albums, music,
/// films and documents apply the same per-device storage limits as files.
/// Commands are the fixed set in `routes::commands`, for devices whose role
/// here lets them change files.
const AREAS: &[&str] = &[
    "files", "search", "recent", "storage", "trash", "system", "shares", "peer", "pairing", "photos", "albums", "library",
    "playback", "tags", "documents", "commands",
];

/// Paths inside those areas that change the computer itself, not files.
const LOCAL_ONLY: &[&str] = &["/api/v1/documents/settings"];

/// The one unsigned call: a device that is not paired yet asking to pair.
const PAIRING_REQUEST: &str = "/api/v1/pairing/request";

/// A stable, non-routable address per remote key, so per-address throttling
/// and logs work for peer-to-peer callers (`fd00::/8`, never loopback).
fn synthetic_addr(key: &str) -> SocketAddr {
    let hash = blake3::hash(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes[0] = 0xfd;
    bytes[1..].copy_from_slice(&hash.as_bytes()[..15]);
    SocketAddr::new(IpAddr::V6(Ipv6Addr::from(bytes)), 0)
}

/// Whether a peer-to-peer request may reach the router at all.
fn admissible<B>(req: &Request<B>) -> bool {
    let path = req.uri().path();
    let Some(rest) = path.strip_prefix("/api/v1/") else { return false };
    let mut segments = rest.split('/');
    let area_ok = segments.next().is_some_and(|a| AREAS.contains(&a)) && !LOCAL_ONLY.iter().any(|p| path.starts_with(p));
    let clean = !rest.split('/').any(|s| s.is_empty() || s == "." || s == ".." || s.to_ascii_lowercase().contains("%2e"));
    let signed = req.headers().contains_key(HDR_DEVICE);
    area_ok
        && clean
        && !req.headers().contains_key(header::AUTHORIZATION)
        && (signed || (path == PAIRING_REQUEST && req.method() == axum::http::Method::POST))
}

async fn handle(app: Router, remote: String, req: Request<Incoming>) -> Response {
    if !admissible(&req) {
        return ApiError::from(NexusError::PermissionDenied("Not available to other devices".into())).into_response();
    }
    let mut req = req.map(Body::new);
    req.extensions_mut().insert(ConnectInfo(synthetic_addr(&remote)));
    req.extensions_mut().insert(P2pPeer(remote));
    match app.oneshot(req).await {
        Ok(res) => res,
        Err(e) => match e {},
    }
}

/// Accepts incoming connections while remote access is on and serves each
/// stream with `app`.
pub fn spawn_server(state: SharedState, app: Router) {
    tokio::spawn(async move {
        while let Some(incoming) = state.net.p2p.accept().await {
            if !state.net.remote_enabled() {
                incoming.refuse();
                continue;
            }
            let (state, app) = (state.clone(), app.clone());
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => serve_connection(state, app, conn).await,
                    Err(e) => tracing::debug!(error = %e, "incoming peer connection failed"),
                }
            });
        }
        tracing::info!("peer-to-peer endpoint closed");
    });
}

async fn serve_connection(state: SharedState, app: Router, conn: Connection) {
    state.net.p2p.track(&conn);
    serve_streams(&state, app, &conn).await;
    state.net.p2p.untrack(&conn);
}

async fn serve_streams(state: &SharedState, app: Router, conn: &Connection) {
    let remote = key_string(&conn.remote_id());
    let mut enabled = state.net.watch_enabled();
    loop {
        tokio::select! {
            stream = conn.accept_bi() => {
                let Ok((send, recv)) = stream else { break };
                let (app, remote) = (app.clone(), remote.clone());
                tokio::spawn(async move {
                    let io = TokioIo::new(tokio::io::join(recv, send));
                    let service = hyper::service::service_fn(move |req: Request<Incoming>| {
                        let (app, remote) = (app.clone(), remote.clone());
                        async move { Ok::<_, Infallible>(handle(app, remote, req).await) }
                    });
                    if let Err(e) = hyper::server::conn::http1::Builder::new().serve_connection(io, service).await {
                        tracing::debug!(error = %e, "peer stream ended with an error");
                    }
                });
            }
            _ = enabled.wait_for(|on| !*on) => {
                conn.close(1u32.into(), b"remote access is off");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, uri: &str, signed: bool, bearer: bool) -> Request<()> {
        let mut b = Request::builder().method(method).uri(uri);
        if signed {
            b = b.header(HDR_DEVICE, "dev");
        }
        if bearer {
            b = b.header(header::AUTHORIZATION, "Bearer x");
        }
        b.body(()).unwrap()
    }

    #[test]
    fn only_device_api_is_admissible() {
        assert!(admissible(&req("POST", "/api/v1/pairing/request", false, false)));
        assert!(admissible(&req("GET", "/api/v1/files?root_id=1", true, false)));
        assert!(admissible(&req("GET", "/api/v1/peer/hello", true, false)));
        // Another device's photos, albums, music and films.
        assert!(admissible(&req("GET", "/api/v1/photos/months", true, false)));
        assert!(admissible(&req("GET", "/api/v1/albums", true, false)));
        assert!(admissible(&req("GET", "/api/v1/library/items?type=film", true, false)));
        assert!(admissible(&req("PUT", "/api/v1/playback/abc", true, false)));
        assert!(admissible(&req("GET", "/api/v1/documents?tag=receipt", true, false)));
        assert!(admissible(&req("PATCH", "/api/v1/documents/abc", true, false)));
        // The fixed commands on the Devices page.
        assert!(admissible(&req("GET", "/api/v1/commands", true, false)));
        assert!(admissible(&req("POST", "/api/v1/commands/rescan", true, false)));
        // Settings that change the computer stay local.
        assert!(!admissible(&req("PUT", "/api/v1/documents/settings", true, false)));
        // Unsigned calls other than the pairing request.
        assert!(!admissible(&req("GET", "/api/v1/files", false, false)));
        assert!(!admissible(&req("POST", "/api/v1/auth/login", false, false)));
        assert!(!admissible(&req("POST", "/api/v1/setup", false, false)));
        // People's sessions never travel between nodes.
        assert!(!admissible(&req("GET", "/api/v1/files", true, true)));
        // Management, relaying and events stay local.
        assert!(!admissible(&req("GET", "/api/v1/devices", true, false)));
        assert!(!admissible(&req("GET", "/api/v1/peers/x/proxy/files", true, false)));
        assert!(!admissible(&req("GET", "/api/v1/events", true, false)));
        // Plugins are installed and run only by people on the computer.
        assert!(!admissible(&req("GET", "/api/v1/plugins", true, false)));
        assert!(!admissible(&req("POST", "/api/v1/plugins/store/checksums", true, false)));
        assert!(!admissible(&req("POST", "/api/v1/plugins/store/checksums/commands/write", true, false)));
        assert!(!admissible(&req("GET", "/api/v1/files/../devices", true, false)));
        assert!(!admissible(&req("GET", "/", true, false)));
        assert_ne!(synthetic_addr("a").ip(), synthetic_addr("b").ip());
        assert!(!synthetic_addr("a").ip().is_loopback());
    }
}
