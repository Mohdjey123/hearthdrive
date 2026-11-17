use crate::error::ApiError;
use crate::p2p::P2pPeer;
use crate::state::{AppState, SharedState};
use axum::extract::{FromRequestParts, OriginalUri};
use axum::http::{HeaderMap, Method, header, request::Parts};
use nexus_common::time::now_ms;
use nexus_common::types::Role;
use nexus_common::{NexusError, Result};
use nexus_devices::SignedRequest;
use nexus_devices::signing::{HDR_ACTING_ROLE, HDR_ACTING_USER, HDR_DEVICE, HDR_NONCE, HDR_SIGNATURE, HDR_TIMESTAMP};
use nexus_security::{Action, hash_token, role_allows, token_matches};
use serde::Serialize;

/// The authenticated caller of a request.
#[derive(Debug, Clone, Serialize)]
pub struct AuthUser {
    pub user_id: Option<String>,
    pub username: String,
    pub role: Role,
    /// Authenticated with the desktop shell's local token.
    pub local: bool,
    /// Set when the caller is a paired device (a signed peer request).
    pub device_id: Option<String>,
}

impl AuthUser {
    pub fn require(&self, action: Action) -> Result<(), ApiError> {
        if role_allows(self.role, action) {
            Ok(())
        } else {
            Err(NexusError::PermissionDenied("Your role does not allow this action".into()).into())
        }
    }

    /// Refuses paired devices: only a person signed in on this node may manage
    /// devices or relay requests to other nodes.
    pub fn require_person(&self) -> Result<(), ApiError> {
        if self.device_id.is_none() {
            Ok(())
        } else {
            Err(NexusError::PermissionDenied("Paired devices cannot do this".into()).into())
        }
    }

    /// Refuses everyone except paired devices.
    pub fn require_device(&self) -> Result<&str, ApiError> {
        self.device_id.as_deref().ok_or_else(|| NexusError::PermissionDenied("Only paired devices can call this".into()).into())
    }
}

pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

pub async fn authenticate(state: &AppState, token: &str) -> Result<AuthUser> {
    if token_matches(token, &state.local_token) {
        let owner = state.db.first_owner().await?;
        return Ok(AuthUser {
            user_id: owner.as_ref().map(|u| u.id.clone()),
            username: owner.map(|u| u.username).unwrap_or_else(|| "local".into()),
            role: Role::Owner,
            local: true,
            device_id: None,
        });
    }
    match state.db.session_user(&hash_token(token)).await? {
        Some(user) => Ok(AuthUser {
            user_id: Some(user.id),
            username: user.username,
            role: user.role,
            local: false,
            device_id: None,
        }),
        None => Err(NexusError::Unauthorized),
    }
}

/// The signature headers of a peer request, before verification.
pub struct SignedHeaders {
    pub device_id: String,
    pub request: SignedRequest,
    pub signature: String,
}

/// Reads the `x-nexus-*` headers. `None` if any is missing or malformed.
pub fn signed_headers(headers: &HeaderMap, method: &Method, path_and_query: &str, target: &str) -> Option<SignedHeaders> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    Some(SignedHeaders {
        device_id: get(HDR_DEVICE)?,
        signature: get(HDR_SIGNATURE)?,
        request: SignedRequest {
            method: method.as_str().to_string(),
            path_and_query: path_and_query.to_string(),
            timestamp_ms: get(HDR_TIMESTAMP)?.parse().ok()?,
            nonce: get(HDR_NONCE)?,
            target_device: target.to_string(),
            acting_user: get(HDR_ACTING_USER).unwrap_or_default(),
            acting_role: get(HDR_ACTING_ROLE).unwrap_or_default(),
        },
    })
}

/// Checks a signature against `public_key`, the clock window and the nonce.
pub fn verify_signed(state: &AppState, signed: &SignedHeaders, public_key: &str) -> bool {
    signed.request.verify(public_key, &signed.signature)
        && state.replay.check(&signed.request.nonce, signed.request.timestamp_ms, now_ms())
}

/// Whether two encodings name the same device key.
pub fn same_key(a: &str, b: &str) -> bool {
    matches!((nexus_network::public_key(a), nexus_network::public_key(b)), (Some(x), Some(y)) if x == y)
}

/// Authenticates a request signed by a paired device, which must also have
/// arrived over a peer-to-peer connection authenticated with the same key.
/// The effective role is the lower of the device's role here and the
/// person's role on their node.
pub async fn authenticate_device(state: &AppState, signed: &SignedHeaders, via: &P2pPeer) -> Result<AuthUser> {
    let device = state.db.get_device(&signed.device_id).await.map_err(|_| NexusError::Unauthorized)?;
    let key = device.public_key.as_deref().filter(|_| !device.is_local).ok_or(NexusError::Unauthorized)?;
    if !same_key(key, &via.0) {
        tracing::warn!(device = %device.name, "rejected a peer request sent from another device's connection");
        return Err(NexusError::Unauthorized);
    }
    if !verify_signed(state, signed, key) {
        tracing::warn!(device = %device.name, "rejected a peer request with a bad or replayed signature");
        return Err(NexusError::Unauthorized);
    }
    let acting = Role::parse(&signed.request.acting_role).unwrap_or(Role::Restricted);
    let person = signed.request.acting_user.trim();
    Ok(AuthUser {
        user_id: None,
        username: if person.is_empty() { device.name.clone() } else { format!("{person} on {}", device.name) },
        role: device.role.least(acting),
        local: false,
        device_id: Some(device.id),
    })
}

/// Path and query as the client sent them, before routers stripped prefixes.
pub fn original_path_and_query(parts: &Parts) -> String {
    let uri = parts.extensions.get::<OriginalUri>().map(|u| &u.0).unwrap_or(&parts.uri);
    uri.path_and_query().map(|pq| pq.as_str().to_string()).unwrap_or_else(|| uri.path().to_string())
}

impl FromRequestParts<SharedState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, Self::Rejection> {
        if let Some(token) = bearer(&parts.headers) {
            return Ok(authenticate(state, token).await?);
        }
        if parts.headers.contains_key(HDR_DEVICE) {
            // Device requests are only accepted over the peer-to-peer network.
            let via = parts.extensions.get::<P2pPeer>().cloned().ok_or(NexusError::Unauthorized)?;
            let pq = original_path_and_query(parts);
            let signed = signed_headers(&parts.headers, &parts.method, &pq, &state.node.device_id)
                .ok_or(NexusError::Unauthorized)?;
            return Ok(authenticate_device(state, &signed, &via).await?);
        }
        Err(NexusError::Unauthorized.into())
    }
}
