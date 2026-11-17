//! Talking to paired devices: signed requests over the peer-to-peer network,
//! presence monitoring, the browse-through proxy, and the joining side of
//! pairing.

use crate::auth::{AuthUser, authenticate, bearer};
use crate::error::ApiError;
use crate::state::SharedState;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, header};
use axum::response::{IntoResponse, Response};
use futures_util::future::join_all;
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::types::{Device, Role};
use nexus_common::{NexusError, Result};
use nexus_database::NewPeer;
use nexus_devices::invite::{hello_message, status_message};
use nexus_devices::signing::{HDR_ACTING_ROLE, HDR_ACTING_ROOTS, HDR_ACTING_USER, HDR_DEVICE, HDR_NONCE, HDR_SIGNATURE, HDR_TIMESTAMP};
use nexus_devices::{NodeIdentity, SignedRequest, random_code, verify_message};
use nexus_network::{P2pNode, PeerKey, RecvStream, SendStream, public_key};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MONITOR_INTERVAL: Duration = Duration::from_secs(20);
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(20);
/// Uploads answer only after the whole body arrived, however long that takes.
pub(crate) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);
const JOIN_POLL_INTERVAL: Duration = Duration::from_secs(2);
const JOIN_DEADLINE_MS: i64 = 20 * 60 * 1000;
/// Largest JSON reply read from a peer.
const MAX_JSON: usize = 1024 * 1024;

/// Host header on peer-to-peer requests (the connection, not the name,
/// identifies the node).
const P2P_HOST: &str = "nexus.p2p";

/// A node to send requests to.
#[derive(Debug, Clone)]
pub struct Target {
    pub device_id: String,
    pub name: String,
    pub key: PeerKey,
    /// `relay:<url>` and `ip:port` hints; the key alone may be enough.
    pub hints: Vec<String>,
}

impl Target {
    pub fn of(device: &Device) -> Result<Self> {
        let key = device
            .public_key
            .as_deref()
            .and_then(public_key)
            .ok_or_else(|| NexusError::internal(format!("{} has no valid key", device.name)))?;
        Ok(Self { device_id: device.id.clone(), name: device.name.clone(), key, hints: device.address_list() })
    }
}

/// Signs and sends requests to other nodes.
pub struct PeerClient {
    identity: Arc<NodeIdentity>,
    self_id: String,
    p2p: Arc<P2pNode>,
}

/// Who a signed request acts for: the person signed in here and their role.
pub struct Acting<'a> {
    pub user: &'a str,
    pub role: Role,
    /// The storage locations the person is limited to, if they are.
    pub roots: Option<&'a [String]>,
}

impl Acting<'static> {
    /// The node itself (presence, pairing status, removal).
    pub const NODE: Acting<'static> = Acting { user: "", role: Role::Restricted, roots: None };
}

impl<'a> Acting<'a> {
    /// Acting for a person, with their limits.
    pub fn of(user: &'a AuthUser) -> Self {
        Acting { user: &user.username, role: user.role, roots: user.allowed_roots.as_deref() }
    }
}

impl PeerClient {
    pub fn new(identity: Arc<NodeIdentity>, self_id: String, p2p: Arc<P2pNode>) -> Self {
        Self { identity, self_id, p2p }
    }

    /// Opens a stream to `target`, reconnecting once if a cached connection
    /// turned out to be dead.
    async fn open(&self, target: &Target) -> Result<(SendStream, RecvStream)> {
        let unreachable = |e: String| NexusError::PeerUnreachable(format!("{} is not reachable right now ({e})", target.name));
        for attempt in 0..2 {
            let conn = self.p2p.connect(target.key, &target.hints).await.map_err(unreachable)?;
            match conn.open_bi().await {
                Ok(stream) => return Ok(stream),
                Err(_) if attempt == 0 => self.p2p.forget(&target.key),
                Err(e) => return Err(unreachable(e.to_string())),
            }
        }
        unreachable!()
    }

    /// Sends one request. `acting` signs it with this node's key; `None`
    /// sends it unsigned (only for the pairing request). Returns the response
    /// once its headers arrive, and the nonce, which some replies sign back.
    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        &self,
        target: &Target,
        method: Method,
        path_and_query: &str,
        mut headers: HeaderMap,
        body: Body,
        acting: Option<Acting<'_>>,
        timeout: Duration,
    ) -> Result<(axum::http::Response<Incoming>, String)> {
        let mut nonce = String::new();
        if let Some(acting) = acting {
            let req = SignedRequest {
                method: method.as_str().to_string(),
                path_and_query: path_and_query.to_string(),
                timestamp_ms: now_ms(),
                nonce: random_code(16),
                target_device: target.device_id.clone(),
                acting_user: acting.user.to_string(),
                acting_role: acting.role.as_str().to_string(),
                acting_roots: acting.roots.map(|r| r.join(",")),
            };
            let value = |s: &str| HeaderValue::from_str(s).map_err(|_| NexusError::invalid("invalid header value"));
            headers.insert(HDR_DEVICE, value(&self.self_id)?);
            headers.insert(HDR_TIMESTAMP, value(&req.timestamp_ms.to_string())?);
            headers.insert(HDR_NONCE, value(&req.nonce)?);
            headers.insert(HDR_SIGNATURE, value(&req.sign(&self.identity))?);
            headers.insert(HDR_ACTING_USER, value(acting.user)?);
            headers.insert(HDR_ACTING_ROLE, value(acting.role.as_str())?);
            if let Some(roots) = &req.acting_roots {
                headers.insert(HDR_ACTING_ROOTS, value(roots)?);
            }
            nonce = req.nonce;
        }
        headers.insert(header::HOST, HeaderValue::from_static(P2P_HOST));

        let mut request = axum::http::Request::builder()
            .method(method)
            .uri(path_and_query)
            .body(body)
            .map_err(|e| NexusError::invalid(format!("invalid request: {e}")))?;
        *request.headers_mut() = headers;

        let exchange = async {
            let (send, recv) = self.open(target).await?;
            let io = TokioIo::new(tokio::io::join(recv, send));
            let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Body>(io)
                .await
                .map_err(|e| NexusError::PeerUnreachable(e.to_string()))?;
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    tracing::debug!(error = %e, "peer stream ended with an error");
                }
            });
            sender.send_request(request).await.map_err(|e| {
                NexusError::PeerUnreachable(format!("{} did not answer ({e})", target.name))
            })
        };
        match tokio::time::timeout(timeout, exchange).await {
            Ok(Ok(res)) => Ok((res, nonce)),
            Ok(Err(e)) => {
                self.p2p.forget(&target.key);
                Err(e)
            }
            Err(_) => {
                self.p2p.forget(&target.key);
                Err(NexusError::PeerUnreachable(format!("{} did not answer in time", target.name)))
            }
        }
    }

    /// Sends a request with an optional JSON body and decodes a JSON reply.
    /// Error replies become the matching error.
    pub async fn call<T: DeserializeOwned>(
        &self,
        target: &Target,
        method: Method,
        path: &str,
        json: Option<&impl Serialize>,
        acting: Option<Acting<'_>>,
        timeout: Duration,
    ) -> Result<(T, String)> {
        self.call_limited(target, method, path, json, acting, timeout, MAX_JSON).await
    }

    /// [`Self::call`] for replies that may be large (folder trees).
    #[allow(clippy::too_many_arguments)]
    pub async fn call_limited<T: DeserializeOwned>(
        &self,
        target: &Target,
        method: Method,
        path: &str,
        json: Option<&impl Serialize>,
        acting: Option<Acting<'_>>,
        timeout: Duration,
        limit: usize,
    ) -> Result<(T, String)> {
        let mut headers = HeaderMap::new();
        let body = match json {
            Some(value) => {
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                Body::from(serde_json::to_vec(value).map_err(NexusError::internal)?)
            }
            None => Body::empty(),
        };
        let (res, nonce) = self.send(target, method, path, headers, body, acting, timeout).await?;
        let status = res.status();
        let bytes = read_body_limited(res, limit)
            .await
            .map_err(|e| NexusError::PeerUnreachable(format!("{}: {e}", target.name)))?;
        if !status.is_success() {
            return Err(peer_error(&target.name, status, &bytes));
        }
        let bytes = if bytes.is_empty() { b"null".to_vec() } else { bytes };
        let value = serde_json::from_slice(&bytes)
            .map_err(|e| NexusError::PeerUnreachable(format!("{} sent an unexpected reply ({e})", target.name)))?;
        Ok((value, nonce))
    }
}

pub(crate) async fn read_body(res: axum::http::Response<Incoming>) -> std::result::Result<Vec<u8>, String> {
    read_body_limited(res, MAX_JSON).await
}

async fn read_body_limited(res: axum::http::Response<Incoming>, limit: usize) -> std::result::Result<Vec<u8>, String> {
    Limited::new(res.into_body(), limit)
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .map_err(|e| e.to_string())
}

/// Turns a peer's JSON error reply into the matching error.
pub(crate) fn peer_error(name: &str, status: axum::http::StatusCode, body: &[u8]) -> NexusError {
    #[derive(Deserialize)]
    struct Body {
        error: Inner,
    }
    #[derive(Deserialize)]
    struct Inner {
        code: String,
        message: String,
    }
    match serde_json::from_slice::<Body>(body) {
        Ok(b) => match b.error.code.as_str() {
            "not_found" => NexusError::NotFound(b.error.message),
            "rate_limited" => NexusError::RateLimited,
            "conflict" => NexusError::Conflict(format!("{name}: {}", b.error.message)),
            "unauthorized" | "permission_denied" => NexusError::PermissionDenied(format!("{name}: {}", b.error.message)),
            "insufficient_storage" => NexusError::InsufficientStorage(format!("{name}: {}", b.error.message)),
            // Worth trying again: the device is busy, offline or had a hiccup.
            "peer_unreachable" | "unavailable" | "io_error" | "internal_error" | "database_error" => {
                NexusError::PeerUnreachable(format!("{name}: {}", b.error.message))
            }
            _ => NexusError::InvalidInput(format!("{name}: {}", b.error.message)),
        },
        Err(_) => NexusError::PeerUnreachable(format!("{name} answered with HTTP {status}")),
    }
}

// ---------------------------------------------------------------------------
// Presence

#[derive(Serialize, Deserialize)]
pub struct HelloReply {
    pub device_id: String,
    pub name: String,
    pub version: String,
    /// The replying node's current address hints.
    pub addresses: Vec<String>,
    /// Signature over the caller's nonce, proving the right node answered.
    pub proof: String,
}

pub fn spawn_peer_monitor(state: SharedState) {
    tokio::spawn(async move {
        loop {
            check_all_peers(&state).await;
            tokio::select! {
                _ = tokio::time::sleep(MONITOR_INTERVAL) => {}
                _ = state.net.peer_wake.notified() => {}
            }
        }
    });
}

pub async fn check_all_peers(state: &SharedState) {
    match state.db.list_peer_devices().await {
        Ok(peers) => {
            join_all(peers.into_iter().map(|p| check_peer(state, p))).await;
        }
        Err(e) => tracing::warn!(error = %e, "could not list paired devices"),
    }
}

async fn check_peer(state: &SharedState, device: Device) {
    match hello(state, &device).await {
        Ok((latency, route, reply)) => {
            let name = reply.name.trim();
            let name = (!name.is_empty() && name.len() <= 64).then_some(name);
            let addresses: Vec<String> =
                reply.addresses.into_iter().filter(|a| nexus_network::valid_hint(a)).take(8).collect();
            if let Err(e) = state
                .db
                .mark_peer_online(&device.id, route.as_deref(), Some(latency), name, Some(&addresses), Some(&reply.version))
                .await
            {
                tracing::warn!(error = %e, "could not record peer presence");
            }
            if device.status != "online" {
                tracing::info!(device = %device.name, route = ?route, latency, "paired device is online");
            }
            state.events.emit(Event::DeviceOnline { device_id: device.id.clone(), latency_ms: latency });
        }
        Err(e) => {
            tracing::debug!(device = %device.name, error = %e, "peer did not answer");
            if device.status != "offline" {
                tracing::info!(device = %device.name, "paired device is offline");
                let _ = state.db.mark_peer_offline(&device.id).await;
                state.events.emit(Event::DeviceOffline { device_id: device.id.clone() });
            }
        }
    }
}

/// Asks a peer to prove it is online. Returns the round-trip time, how the
/// connection flows (`direct 1.2.3.4:5` or `relay https://…`) and the reply.
async fn hello(state: &SharedState, device: &Device) -> Result<(i64, Option<String>, HelloReply)> {
    let target = Target::of(device)?;
    let key = device.public_key.as_deref().unwrap_or_default();
    let started = Instant::now();
    let (reply, nonce): (HelloReply, String) = state
        .peers
        .call(&target, Method::GET, "/api/v1/peer/hello", None::<&()>, Some(Acting::NODE), HELLO_TIMEOUT)
        .await?;
    let elapsed = started.elapsed().as_millis() as i64;
    if reply.device_id != device.id || !verify_message(key, &hello_message(&nonce, &device.id), &reply.proof) {
        return Err(NexusError::PeerUnreachable(format!("the reply did not come from {}", device.name)));
    }
    let route = state.net.p2p.route(&target.key);
    let latency = route.as_ref().map(|r| r.rtt_ms as i64).filter(|ms| *ms > 0).unwrap_or(elapsed);
    Ok((latency, route.map(|r| format!("{} {}", r.kind, r.remote)), reply))
}

// ---------------------------------------------------------------------------
// Proxy

/// API areas another node's files can be browsed through.
const PROXY_AREAS: &[&str] = &["files", "search", "recent", "storage", "trash", "system"];

const REQUEST_HEADERS: &[HeaderName] = &[
    header::CONTENT_TYPE,
    header::CONTENT_LENGTH,
    header::RANGE,
    header::IF_RANGE,
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
    header::ACCEPT,
];

const RESPONSE_HEADERS: &[HeaderName] = &[
    header::CONTENT_TYPE,
    header::CONTENT_LENGTH,
    header::CONTENT_RANGE,
    header::ACCEPT_RANGES,
    header::CONTENT_DISPOSITION,
    header::CACHE_CONTROL,
    header::ETAG,
    header::LAST_MODIFIED,
];

/// The policy every proxied response gets (the same one as local file content).
const PROXY_CSP: &str = "sandbox; default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'";

fn is_ticket_content(rest: &str, query: Option<&str>) -> bool {
    let parts: Vec<&str> = rest.split('/').collect();
    parts.len() == 3
        && parts[0] == "files"
        && parts[2] == "content"
        && query.is_some_and(|q| q.split('&').any(|p| p.starts_with("ticket=")))
}

/// `ANY /api/v1/peers/{device_id}/proxy/{*rest}`: forwards a request from a
/// person signed in here to a paired device, signed with this node's key.
/// Media URLs carry the peer's own content ticket instead of a session.
pub async fn proxy(
    State(s): State<SharedState>,
    Path((device_id, _)): Path<(String, String)>,
    req: Request,
) -> Result<Response, ApiError> {
    let raw_path = req.uri().path().to_string();
    let query = req.uri().query().map(str::to_string);
    let prefix = format!("/peers/{device_id}/proxy/");
    let rest = raw_path.strip_prefix(&prefix).ok_or_else(|| NexusError::invalid("Malformed proxy path"))?.to_string();

    let segments: Vec<&str> = rest.split('/').collect();
    if segments.iter().any(|s| s.is_empty() || *s == "." || *s == ".." || s.contains('%') && s.to_ascii_lowercase().contains("%2e"))
        || !PROXY_AREAS.contains(&segments[0])
    {
        return Err(NexusError::PermissionDenied("This request cannot be sent to another device".into()).into());
    }

    let caller: AuthUser = match bearer(req.headers()) {
        Some(token) => authenticate(&s, token).await?,
        None if is_ticket_content(&rest, query.as_deref()) => AuthUser {
            user_id: None,
            username: "media".into(),
            role: Role::Viewer,
            local: false,
            device_id: None,
            allowed_roots: None,
        },
        None => return Err(NexusError::Unauthorized.into()),
    };
    caller.require_person()?;

    let device = s.db.get_device(&device_id).await?;
    if device.is_local || device.public_key.is_none() {
        return Err(NexusError::NotFound("paired device".into()).into());
    }
    let target = Target::of(&device)?;
    let path_and_query = match &query {
        Some(q) => format!("/api/v1/{rest}?{q}"),
        None => format!("/api/v1/{rest}"),
    };

    let method = req.method().clone();
    let mut headers = HeaderMap::new();
    for name in REQUEST_HEADERS {
        if let Some(value) = req.headers().get(name) {
            headers.insert(name.clone(), value.clone());
        }
    }
    let has_body = !matches!(method, Method::GET | Method::HEAD | Method::DELETE);
    let body = if has_body { req.into_body() } else { Body::empty() };
    let acting = Acting::of(&caller);
    // A request with a body (an upload) is answered only once the peer has
    // received all of it, so it gets a transfer-sized time limit.
    let timeout = if has_body { TRANSFER_TIMEOUT } else { CALL_TIMEOUT };

    let res = match s.peers.send(&target, method, &path_and_query, headers, body, Some(acting), timeout).await {
        Ok((res, _)) => res,
        Err(e) => {
            s.net.peer_wake.notify_one();
            return Err(e.into());
        }
    };

    let mut response = Response::builder().status(res.status());
    if let Some(headers) = response.headers_mut() {
        copy_headers(res.headers(), headers);
        // Whatever the device sends is shown on this node's origin, so it is
        // always sandboxed here: a device cannot serve a page that reads the
        // session of someone signed in to this node.
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(PROXY_CSP));
        headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    }
    Ok(response.body(Body::new(res.into_body())).map_err(NexusError::internal)?.into_response())
}

fn copy_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for name in RESPONSE_HEADERS {
        if let Some(value) = from.get(name) {
            to.insert(name.clone(), value.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Joining another node

#[derive(Debug, Clone, Serialize)]
pub struct JoinState {
    pub id: String,
    pub host_device_id: String,
    pub host_name: String,
    pub host_fingerprint: String,
    #[serde(skip)]
    pub host_public_key: String,
    /// How the host was reached (`direct …` or `relay …`), if known.
    pub route: Option<String>,
    #[serde(skip)]
    pub addresses: Vec<String>,
    /// `waiting` | `approved` | `rejected` | `expired` | `failed`
    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
}

impl JoinState {
    fn target(&self) -> Result<Target> {
        Ok(Target {
            device_id: self.host_device_id.clone(),
            name: self.host_name.clone(),
            key: public_key(&self.host_public_key).ok_or_else(|| NexusError::invalid("invalid host key"))?,
            hints: self.addresses.clone(),
        })
    }
}

#[derive(Serialize)]
pub struct PairRequestBody {
    pub code: String,
    pub device_id: String,
    pub name: String,
    pub device_type: String,
    pub public_key: String,
    pub addresses: Vec<String>,
    pub version: String,
    pub proof: String,
}

#[derive(Deserialize)]
pub struct PairAck {
    pub request_id: String,
    pub host_device_id: String,
    pub host_name: String,
    pub signature: String,
}

/// Sends the pairing request to the host named in an invitation. The
/// connection itself verifies that the host holds the invitation's key.
pub async fn send_pair_request(s: &SharedState, target: &Target, body: &PairRequestBody) -> Result<PairAck> {
    let result = s
        .peers
        .call::<PairAck>(target, Method::POST, "/api/v1/pairing/request", Some(body), None, CALL_TIMEOUT)
        .await;
    match result {
        Ok((ack, _)) => Ok(ack),
        Err(NexusError::PeerUnreachable(e)) => Err(NexusError::PeerUnreachable(format!(
            "Could not reach {}. Make sure NEXUS is running there and both computers are online. ({e})",
            target.name
        ))),
        Err(e) => Err(e),
    }
}

fn set_join(s: &SharedState, id: &str, status: &str, error: Option<String>) {
    let mut joins = s.joins.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(j) = joins.get_mut(id) {
        if j.status == status && j.error == error {
            return;
        }
        j.status = status.to_string();
        j.error = error;
    }
    drop(joins);
    s.events.emit(Event::PairingUpdated { id: id.to_string(), status: status.to_string() });
}

#[derive(Deserialize)]
struct StatusReply {
    status: String,
    signature: String,
}

/// Polls the host until the owner approves or rejects this device.
pub fn spawn_join_watcher(s: SharedState, join: JoinState) {
    tokio::spawn(async move {
        let deadline = join.created_at + JOIN_DEADLINE_MS;
        let Ok(target) = join.target() else { return };
        let path = format!("/api/v1/pairing/requests/{}/status", join.id);
        loop {
            tokio::time::sleep(JOIN_POLL_INTERVAL).await;
            if now_ms() > deadline {
                set_join(&s, &join.id, "expired", Some("Nobody approved the request in time".into()));
                return;
            }
            let reply = match s
                .peers
                .call::<StatusReply>(&target, Method::GET, &path, None::<&()>, Some(Acting::NODE), CALL_TIMEOUT)
                .await
            {
                Ok((reply, _)) => reply,
                Err(NexusError::PeerUnreachable(_)) => continue, // Host briefly unreachable: keep waiting.
                Err(e) => {
                    set_join(&s, &join.id, "failed", Some(e.to_string()));
                    return;
                }
            };
            if !verify_message(&join.host_public_key, &status_message(&join.id, &reply.status), &reply.signature) {
                set_join(&s, &join.id, "failed", Some("The host's reply was not signed by the expected device".into()));
                return;
            }
            match reply.status.as_str() {
                "pending" => continue,
                "approved" => {
                    let peer = NewPeer {
                        id: &join.host_device_id,
                        name: &join.host_name,
                        device_type: "windows",
                        public_key: &join.host_public_key,
                        addresses: &join.addresses,
                        role: Role::Editor,
                        version: None,
                    };
                    match s.db.upsert_paired_device(&peer).await {
                        Ok(()) => {
                            tracing::info!(host = %join.host_name, "joined; device paired");
                            let _ = s
                                .db
                                .log_activity("device.paired", Some(&join.host_name), None, Some(&join.host_device_id))
                                .await;
                            s.events.emit(Event::DeviceChanged { device_id: join.host_device_id.clone() });
                            set_join(&s, &join.id, "approved", None);
                            s.net.peer_wake.notify_one();
                        }
                        Err(e) => set_join(&s, &join.id, "failed", Some(e.to_string())),
                    }
                    return;
                }
                "rejected" => {
                    set_join(&s, &join.id, "rejected", Some(format!("{} declined the request", join.host_name)));
                    return;
                }
                other => {
                    set_join(&s, &join.id, "expired", Some(format!("The request is {other}")));
                    return;
                }
            }
        }
    });
}

/// How the current connection to `key` flows, for display.
pub fn route_label(s: &SharedState, key: &PeerKey) -> Option<String> {
    s.net.p2p.route(key).map(|r| format!("{} {}", r.kind, r.remote))
}
