//! Paired devices and pairing.
//!
//! Host side: a person creates an invitation (QR / link), the joining device
//! presents the code with proof that it holds its key, and the person approves
//! it after comparing fingerprints. Joining side: paste the link, send the
//! request, wait for approval. Both nodes then know each other's public key
//! and accept each other's signed requests until one removes the other.

use super::audit;
use crate::auth::{AuthUser, same_key, signed_headers, verify_signed};
use crate::error::ApiResult;
use crate::p2p::P2pPeer;
use crate::peers::{
    Acting, HelloReply, JoinState, PairRequestBody, Target, route_label, send_pair_request, spawn_join_watcher,
};
use crate::state::SharedState;
use axum::Json;
use axum::extract::{ConnectInfo, Extension, OriginalUri, Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::types::{Device, PairingRequest, Role};
use nexus_common::{NexusError, VERSION};
use nexus_database::{NewPairingRequest, NewPeer};
use nexus_devices::invite::{ack_message, request_proof_message, status_message};
use nexus_devices::signing::HDR_NONCE;
use nexus_devices::{PairingInvite, fingerprint, random_code, verify_message};
use nexus_security::{Action, hash_token};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Duration;

const INVITE_TTL_MS: i64 = 10 * 60 * 1000;
const MAX_REQUESTS_PER_INVITE: i64 = 5;

#[derive(Serialize)]
pub struct DeviceView {
    #[serde(flatten)]
    device: Device,
    online: bool,
    fingerprint: Option<String>,
    addresses: Vec<String>,
    /// How traffic to this device flows right now (`direct …` / `relay …`).
    route: Option<String>,
    /// Storage locations here that this device may see (`null` = all).
    allowed_roots: Option<Vec<String>>,
    roots: usize,
    indexed_bytes: i64,
}

pub async fn list(State(s): State<SharedState>, _user: AuthUser) -> ApiResult<Json<Vec<DeviceView>>> {
    let roots = s.db.list_roots().await?;
    let mut out = Vec::new();
    for device in s.db.list_devices().await? {
        let mut indexed_bytes = 0;
        let mut count = 0;
        for root in roots.iter().filter(|r| r.device_id == device.id) {
            count += 1;
            indexed_bytes += s.db.root_stats(&root.id).await?.bytes;
        }
        let online = device.is_local || device.status == "online";
        let fingerprint = device.public_key.as_deref().and_then(fingerprint);
        let addresses = if device.is_local { s.net.advertised() } else { device.address_list() };
        let route = match device.public_key.as_deref().and_then(nexus_network::public_key) {
            Some(key) if !device.is_local => route_label(&s, &key),
            _ => None,
        };
        let allowed_roots = device.allowed_root_list();
        out.push(DeviceView { device, online, fingerprint, addresses, route, allowed_roots, roots: count, indexed_bytes });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct UpdateDevice {
    name: Option<String>,
    role: Option<Role>,
    /// Limit the device to these storage locations…
    allowed_roots: Option<Vec<String>>,
    /// …or let it see all of them again.
    all_roots: Option<bool>,
}

/// Renames this node, or changes what a paired device may do here.
pub async fn update(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<UpdateDevice>,
) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    let device = s.db.get_device(&id).await?;
    if let Some(name) = req.name {
        let name = name.trim();
        if !device.is_local {
            return Err(NexusError::invalid("Rename a paired device on that device").into());
        }
        if name.is_empty() || name.len() > 64 {
            return Err(NexusError::invalid("Use a name of up to 64 characters").into());
        }
        s.db.rename_device(&id, name).await?;
    }
    if let Some(role) = req.role {
        if device.is_local {
            return Err(NexusError::invalid("This computer has no device role").into());
        }
        if matches!(role, Role::Owner) {
            return Err(NexusError::invalid("A device cannot be an owner").into());
        }
        s.db.set_device_role(&id, role).await?;
        audit(&s, &user, "device.role.changed", Some(&format!("{} → {}", device.name, role.as_str()))).await;
    }
    if req.all_roots == Some(true) || req.allowed_roots.is_some() {
        if device.is_local {
            return Err(NexusError::invalid("This computer always sees its own storage").into());
        }
        let chosen = if req.all_roots == Some(true) { None } else { req.allowed_roots };
        if let Some(list) = &chosen {
            let local: Vec<String> = s
                .db
                .list_roots()
                .await?
                .into_iter()
                .filter(|r| r.device_id == s.node.device_id)
                .map(|r| r.id)
                .collect();
            if list.iter().any(|id| !local.contains(id)) {
                return Err(NexusError::invalid("Unknown storage location").into());
            }
        }
        s.db.set_device_roots(&id, chosen.as_deref()).await?;
        let summary = match &chosen {
            None => "all storage".to_string(),
            Some(list) => format!("{} storage location(s)", list.len()),
        };
        audit(&s, &user, "device.access.changed", Some(&format!("{} → {summary}", device.name))).await;
    }
    s.events.emit(Event::DeviceChanged { device_id: id });
    s.introductions.notify_one();
    Ok(StatusCode::NO_CONTENT)
}

/// Removes a paired device. Its requests are refused immediately; the device
/// is told (best effort) so it forgets this node too.
pub async fn remove(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    let device = s.db.get_device(&id).await?;
    if device.is_local {
        return Err(NexusError::invalid("This computer cannot remove itself").into());
    }
    if device.introduced_by.is_some() {
        // Removed by hand: introductions must not bring it back.
        super::introductions::decline(&s, &id).await;
    }
    s.db.delete_peer_device(&id).await?;
    super::introductions::forget_all_from(&s, &id).await;
    audit(&s, &user, "device.removed", Some(&device.name)).await;
    s.events.emit(Event::DeviceRemoved { device_id: id.clone() });
    s.introductions.notify_one();

    let notify = s.clone();
    tokio::spawn(async move {
        if let Ok(target) = Target::of(&device) {
            let _ = notify
                .peers
                .send(&target, Method::POST, "/api/v1/peer/unpair", HeaderMap::new(), Default::default(), Some(Acting::NODE), Duration::from_secs(8))
                .await;
            notify.net.p2p.forget(&target.key);
        }
    });
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Host side

#[derive(Serialize)]
pub struct InviteResponse {
    session_id: String,
    link: String,
    expires_at: i64,
    fingerprint: String,
    addresses: Vec<String>,
}

pub async fn create_invite(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<InviteResponse>> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    if !s.net.remote_enabled() {
        return Err(NexusError::Conflict("Turn on remote access first, so the other device can connect to this computer.".into()).into());
    }
    let addresses = s.net.advertised();
    if addresses.is_empty() && !s.net.snapshot().status.address_lookup {
        return Err(NexusError::Conflict("This computer has no network address yet. Check its network connection and try again.".into()).into());
    }
    // Keep the link short so its QR code stays easy to scan: the relay,
    // then a few direct addresses (the rest are learned after pairing).
    let addresses = compact_hints(addresses);
    let code = random_code(16);
    let expires_at = now_ms() + INVITE_TTL_MS;
    let session_id = s.db.create_pairing_session(&hash_token(&code), user.user_id.as_deref(), expires_at).await?;
    let device = s.db.get_device(&s.node.device_id).await?;
    let invite = PairingInvite {
        v: 1,
        device_id: device.id,
        name: device.name,
        public_key: s.identity.public_key().to_string(),
        addrs: addresses.clone(),
        code,
        expires_at,
    };
    audit(&s, &user, "device.invite.created", None).await;
    Ok(Json(InviteResponse {
        session_id,
        link: invite.to_link(),
        expires_at,
        fingerprint: s.identity.fingerprint().to_string(),
        addresses,
    }))
}

/// The relay plus at most three IPv4 and one IPv6 direct address.
fn compact_hints(hints: Vec<String>) -> Vec<String> {
    let (mut v4, mut v6) = (0, 0);
    hints
        .into_iter()
        .filter(|h| {
            if h.starts_with("relay:") {
                true
            } else if h.starts_with('[') {
                v6 += 1;
                v6 <= 1
            } else {
                v4 += 1;
                v4 <= 3
            }
        })
        .collect()
}

pub async fn cancel_invite(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    s.db.close_pairing_session(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct RequestView {
    #[serde(flatten)]
    request: PairingRequest,
    fingerprint: Option<String>,
    addresses: Vec<String>,
}

pub async fn list_requests(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<RequestView>>> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    let out = s
        .db
        .pending_pairing_requests()
        .await?
        .into_iter()
        .map(|r| RequestView { fingerprint: fingerprint(&r.public_key), addresses: r.address_list(), request: r })
        .collect();
    Ok(Json(out))
}

#[derive(Deserialize, Default)]
pub struct ApproveBody {
    role: Option<Role>,
}

pub async fn approve(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    body: Option<Json<ApproveBody>>,
) -> ApiResult<Json<Value>> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    let role = body.and_then(|b| b.0.role).unwrap_or(Role::Editor);
    if matches!(role, Role::Owner) {
        return Err(NexusError::invalid("A device cannot be an owner").into());
    }
    let req = s.db.get_pairing_request(&id).await?;
    if req.status != "pending" {
        return Err(NexusError::Conflict(format!("This request is already {}", req.status)).into());
    }
    // A request may not take over a device that is already paired with a
    // different key (it would inherit that device's access here).
    if let Ok(existing) = s.db.get_device(&req.device_id).await
        && (existing.is_local || !existing.public_key.as_deref().is_some_and(|k| same_key(k, &req.public_key)))
    {
        return Err(NexusError::Conflict(format!(
            "Another device named {} is already paired with this id. Remove it under Devices first if it was reinstalled.",
            existing.name
        ))
        .into());
    }
    let addresses = req.address_list();
    s.db.upsert_paired_device(&NewPeer {
        id: &req.device_id,
        name: &req.name,
        device_type: &req.device_type,
        public_key: &req.public_key,
        addresses: &addresses,
        role,
        version: req.version.as_deref(),
    })
    .await?;
    if !s.db.decide_pairing_request(&id, "approved", user.user_id.as_deref()).await? {
        return Err(NexusError::Conflict("This request was already decided".into()).into());
    }
    s.db.close_pairing_session(&req.session_id).await?;
    audit(&s, &user, "device.paired", Some(&req.name)).await;
    super::introductions::undecline(&s, &req.device_id).await;
    s.events.emit(Event::DeviceChanged { device_id: req.device_id.clone() });
    s.net.peer_wake.notify_one();
    s.introductions.notify_one();
    Ok(Json(json!({ "device_id": req.device_id })))
}

pub async fn reject(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    let req = s.db.get_pairing_request(&id).await?;
    s.db.decide_pairing_request(&id, "rejected", user.user_id.as_deref()).await?;
    audit(&s, &user, "device.pairing.rejected", Some(&req.name)).await;
    s.events.emit(Event::DeviceChanged { device_id: req.device_id });
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct IncomingPairRequest {
    code: String,
    device_id: String,
    name: String,
    device_type: String,
    public_key: String,
    #[serde(default)]
    addresses: Vec<String>,
    version: Option<String>,
    proof: String,
}

/// `POST /pairing/request`: no session; gated by the invitation code, proof of
/// key possession (the connection's key and a signature), and the owner's
/// approval.
pub async fn incoming_request(
    State(s): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    via: Option<Extension<P2pPeer>>,
    Json(req): Json<IncomingPairRequest>,
) -> ApiResult<Json<Value>> {
    let Some(Extension(via)) = via else {
        return Err(NexusError::PermissionDenied("Pairing requests come from other devices only".into()).into());
    };
    s.throttle.check(peer.ip())?;
    let Some(session) = s.db.open_pairing_session(&hash_token(req.code.trim())).await? else {
        s.throttle.fail(peer.ip());
        return Err(NexusError::PermissionDenied("This pairing code is invalid or has expired. Create a new one.".into()).into());
    };
    if s.db.count_session_requests(&session.id).await? >= MAX_REQUESTS_PER_INVITE {
        return Err(NexusError::Conflict("This invitation has been used too many times. Create a new one.".into()).into());
    }

    let name = req.name.trim();
    let addresses: Vec<String> = req.addresses.into_iter().filter(|a| nexus_network::valid_hint(a)).take(8).collect();
    let id_ok = (1..=64).contains(&req.device_id.len())
        && req.device_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let version_ok = req.version.as_deref().is_none_or(|v| v.len() <= 32 && v.chars().all(|c| c.is_ascii_graphic()));
    if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) || !id_ok || !version_ok {
        return Err(NexusError::invalid("Invalid device details").into());
    }
    if !matches!(req.device_type.as_str(), "windows" | "linux" | "macos" | "android" | "ios" | "web") {
        return Err(NexusError::invalid("Unknown device type").into());
    }
    if req.device_id == s.node.device_id {
        return Err(NexusError::invalid("A device cannot pair with itself").into());
    }
    if fingerprint(&req.public_key).is_none() {
        return Err(NexusError::invalid("Invalid device key").into());
    }
    if !same_key(&req.public_key, &via.0) {
        s.throttle.fail(peer.ip());
        return Err(NexusError::PermissionDenied("The device's key does not match its connection".into()).into());
    }
    let proof = request_proof_message(&s.node.device_id, req.code.trim(), &req.device_id, &req.public_key);
    if !verify_message(&req.public_key, &proof, &req.proof) {
        s.throttle.fail(peer.ip());
        return Err(NexusError::PermissionDenied("The device could not prove it owns its key".into()).into());
    }

    let ip = nexus_network::key_short(&via.0);
    let created = s
        .db
        .insert_pairing_request(&NewPairingRequest {
            session_id: &session.id,
            device_id: &req.device_id,
            name,
            device_type: &req.device_type,
            public_key: &req.public_key,
            addresses: &addresses,
            version: req.version.as_deref(),
            remote_ip: Some(&ip),
        })
        .await?;
    s.throttle.clear(peer.ip());
    let _ = s.db.log_activity("device.pairing.requested", Some(name), None, Some(&req.device_id)).await;
    s.events.emit(Event::PairingRequested { request_id: created.id.clone(), name: name.to_string() });
    tracing::info!(device = name, %ip, "pairing requested");

    let device = s.db.get_device(&s.node.device_id).await?;
    Ok(Json(json!({
        "request_id": created.id,
        "host_device_id": s.node.device_id,
        "host_name": device.name,
        "signature": s.identity.sign(&ack_message(&created.id, &req.device_id, &req.public_key, req.code.trim())),
    })))
}

/// `GET /pairing/requests/{id}/status`, signed with the requesting device's
/// key (it is not paired yet, so the key comes from the request itself).
pub async fn request_status(
    State(s): State<SharedState>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    via: Option<Extension<P2pPeer>>,
) -> ApiResult<Json<Value>> {
    let Some(Extension(via)) = via else { return Err(NexusError::Unauthorized.into()) };
    let req = s.db.get_pairing_request(&id).await?;
    if !same_key(&req.public_key, &via.0) {
        return Err(NexusError::Unauthorized.into());
    }
    let pq = uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| uri.path().to_string());
    let signed = signed_headers(&headers, &method, &pq, &s.node.device_id).ok_or(NexusError::Unauthorized)?;
    if signed.device_id != req.device_id || !verify_signed(&s, &signed, &req.public_key) {
        return Err(NexusError::Unauthorized.into());
    }
    Ok(Json(json!({
        "status": req.status,
        "signature": s.identity.sign(&status_message(&req.id, &req.status)),
    })))
}

// ---------------------------------------------------------------------------
// Joining side

#[derive(Deserialize)]
pub struct JoinBody {
    link: String,
}

pub async fn join(State(s): State<SharedState>, user: AuthUser, Json(body): Json<JoinBody>) -> ApiResult<Json<JoinState>> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    let invite = PairingInvite::parse(&body.link)?;
    if invite.expires_at < now_ms() {
        return Err(NexusError::invalid("This pairing link has expired. Create a new one on the other computer.").into());
    }
    if invite.device_id == s.node.device_id {
        return Err(NexusError::invalid("This link is for this computer. Open it on the device you want to add.").into());
    }
    let id_ok = (1..=64).contains(&invite.device_id.len())
        && invite.device_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let name = invite.name.trim();
    if !id_ok || name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
        return Err(NexusError::invalid("This pairing link is incomplete").into());
    }
    if let Ok(existing) = s.db.get_device(&invite.device_id).await
        && !existing.public_key.as_deref().is_some_and(|k| same_key(k, &invite.public_key))
    {
        return Err(NexusError::Conflict(format!(
            "Another device named {} is already paired with this id. Remove it under Devices first if it was reinstalled.",
            existing.name
        ))
        .into());
    }
    let me = s.db.get_device(&s.node.device_id).await?;
    let request = PairRequestBody {
        code: invite.code.clone(),
        device_id: me.id.clone(),
        name: me.name.clone(),
        device_type: platform_type().into(),
        public_key: s.identity.public_key().to_string(),
        addresses: s.net.advertised(),
        version: VERSION.into(),
        proof: s.identity.sign(&request_proof_message(&invite.device_id, &invite.code, &me.id, s.identity.public_key())),
    };
    let key = nexus_network::public_key(&invite.public_key).ok_or_else(|| NexusError::invalid("This pairing link is incomplete"))?;
    let addresses: Vec<String> = invite.addrs.iter().filter(|a| nexus_network::valid_hint(a)).take(8).cloned().collect();
    let target = Target { device_id: invite.device_id.clone(), name: invite.name.clone(), key, hints: addresses.clone() };
    let ack = send_pair_request(&s, &target, &request).await?;
    let expected = ack_message(&ack.request_id, &me.id, s.identity.public_key(), &invite.code);
    if ack.host_device_id != invite.device_id || !verify_message(&invite.public_key, &expected, &ack.signature) {
        return Err(NexusError::PermissionDenied(
            "The computer that answered is not the one in the pairing link. Nothing was paired.".into(),
        )
        .into());
    }
    let join = JoinState {
        id: ack.request_id.clone(),
        host_device_id: invite.device_id.clone(),
        host_name: if ack.host_name.trim().is_empty() { invite.name.clone() } else { ack.host_name.clone() },
        host_fingerprint: fingerprint(&invite.public_key).unwrap_or_default(),
        host_public_key: invite.public_key.clone(),
        route: route_label(&s, &key),
        addresses,
        status: "waiting".into(),
        error: None,
        created_at: now_ms(),
    };
    s.joins.lock().unwrap_or_else(|e| e.into_inner()).insert(join.id.clone(), join.clone());
    audit(&s, &user, "device.join.requested", Some(&join.host_name)).await;
    spawn_join_watcher(s.clone(), join.clone());
    Ok(Json(join))
}

pub async fn join_status(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<JoinState>> {
    user.require_person()?;
    let join = s.joins.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
    Ok(Json(join.ok_or_else(|| NexusError::NotFound("join request".into()))?))
}

fn platform_type() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "android" => "android",
        _ => "linux",
    }
}

// ---------------------------------------------------------------------------
// Peer endpoints (signed by a paired device)

pub async fn hello(State(s): State<SharedState>, user: AuthUser, headers: HeaderMap) -> ApiResult<Json<HelloReply>> {
    let caller = user.require_device()?.to_string();
    let nonce = headers.get(HDR_NONCE).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let _ = s.db.mark_peer_online(&caller, None, None, None, None, None).await;
    let me = s.db.get_device(&s.node.device_id).await?;
    Ok(Json(HelloReply {
        proof: s.identity.sign(&nexus_devices::invite::hello_message(nonce, &me.id)),
        device_id: me.id,
        name: me.name,
        version: VERSION.into(),
        addresses: s.net.advertised(),
    }))
}

/// The caller removed this node, so this node forgets the caller too.
pub async fn unpair(State(s): State<SharedState>, user: AuthUser) -> ApiResult<StatusCode> {
    let caller = user.require_device()?.to_string();
    let device = s.db.get_device(&caller).await?;
    if device.introduced_by.is_some() {
        super::introductions::decline(&s, &caller).await;
    }
    s.db.delete_peer_device(&caller).await?;
    super::introductions::forget_all_from(&s, &caller).await;
    s.introductions.notify_one();
    if let Some(key) = device.public_key.as_deref().and_then(nexus_network::public_key) {
        s.net.p2p.forget(&key);
    }
    let _ = s.db.log_activity("device.unpaired.by_peer", Some(&device.name), None, Some(&caller)).await;
    s.events.emit(Event::DeviceRemoved { device_id: caller });
    Ok(StatusCode::NO_CONTENT)
}
