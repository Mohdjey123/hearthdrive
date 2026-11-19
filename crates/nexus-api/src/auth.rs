use crate::error::ApiError;
use crate::p2p::P2pPeer;
use crate::state::{AppState, SharedState};
use axum::extract::{FromRequestParts, OriginalUri};
use axum::http::{HeaderMap, Method, header, request::Parts};
use nexus_common::time::now_ms;
use nexus_common::types::{Role, User};
use nexus_common::{NexusError, Result};
use nexus_database::{FolderAccess, FolderGrant};
use nexus_devices::SignedRequest;
use nexus_devices::signing::{HDR_ACTING_ROLE, HDR_ACTING_ROOTS, HDR_ACTING_USER, HDR_DEVICE, HDR_NONCE, HDR_SIGNATURE, HDR_TIMESTAMP};
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
    /// Storage locations the caller is limited to (`None` = all): set for
    /// people and paired devices the owner limited. A limited person's list
    /// travels with their requests to paired devices, which apply it too.
    #[serde(skip)]
    pub allowed_roots: Option<Vec<String>>,
    /// Folders shared with the caller. They add to the storage locations
    /// above, never beyond what the role allows.
    #[serde(skip)]
    pub shared: Vec<FolderGrant>,
    /// Signed in with a token for programs: such a caller may not manage
    /// people, tokens or sign-in.
    #[serde(skip)]
    pub via_token: bool,
}

/// What a request does with a path, for checking it against shared folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// List, open or download.
    Read,
    /// Add, replace or change what is inside.
    Write,
    /// Rename, move or delete the item itself: never a shared folder itself.
    Change,
    /// Add something new (a folder or a file under a free name).
    Add,
}

/// One grant per shared folder: someone given a folder both directly and
/// through a group gets the most it allows (edit, then view, then add).
fn strongest(grants: Vec<FolderGrant>) -> Vec<FolderGrant> {
    let rank = |a: FolderAccess| match a {
        FolderAccess::Edit => 3,
        FolderAccess::View => 2,
        FolderAccess::Add => 1,
    };
    let mut out: Vec<FolderGrant> = Vec::with_capacity(grants.len());
    for g in grants {
        match out.iter_mut().find(|o| o.folder_id == g.folder_id) {
            Some(o) if rank(g.access) > rank(o.access) => *o = g,
            Some(_) => {}
            None => out.push(g),
        }
    }
    out
}

/// Whether `path` is `base` or inside it. Paths are case-insensitive.
fn within(path: &str, base: &str) -> bool {
    if base.is_empty() {
        return true;
    }
    let (path, base) = (path.to_lowercase(), base.to_lowercase());
    path == base || path.strip_prefix(&base).is_some_and(|rest| rest.starts_with('/'))
}

impl AuthUser {
    /// A person signed in on this node, with their current role and limits.
    pub fn person(user: User) -> Self {
        let allowed_roots = user.limited_roots();
        Self {
            user_id: Some(user.id),
            username: user.username,
            role: user.role,
            local: false,
            device_id: None,
            allowed_roots,
            shared: Vec::new(),
            via_token: false,
        }
    }

    /// Adds the folders shared with this person (directly or through a
    /// group they are in) or device.
    pub async fn with_shared(mut self, state: &AppState) -> Result<Self> {
        let members = match (&self.device_id, &self.user_id) {
            (Some(device), _) => vec![format!("device:{device}")],
            (None, Some(user)) if !self.local => {
                let mut members = vec![format!("user:{user}")];
                members.extend(state.db.groups_of_user(user).await?.into_iter().map(|g| format!("group:{g}")));
                members
            }
            _ => return Ok(self),
        };
        self.shared = strongest(state.db.grants_for(&members).await?);
        Ok(self)
    }

    /// Who owns something the caller started (a resumable upload), in a form
    /// no one else can take: a paired device could name itself after a
    /// person here, but never gets their account id.
    pub fn owner_key(&self) -> String {
        match (&self.device_id, &self.user_id) {
            (Some(device), _) => format!("device:{device}:{}", self.username),
            (None, Some(user)) => format!("user:{user}"),
            (None, None) => format!("local:{}", self.username),
        }
    }

    /// Limited to some storage locations.
    pub fn is_limited(&self) -> bool {
        self.allowed_roots.is_some()
    }

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

    /// Whether the caller may see this storage location at all.
    pub fn sees_root(&self, root_id: &str) -> bool {
        self.allowed_roots.as_ref().is_none_or(|allowed| allowed.iter().any(|r| r == root_id))
    }

    /// Hides storage locations the caller was not given: they look missing.
    pub fn require_root(&self, root_id: &str) -> Result<(), ApiError> {
        if self.sees_root(root_id) {
            Ok(())
        } else {
            Err(NexusError::NotFound("Storage location".into()).into())
        }
    }

    /// Whether the caller may see anything in this storage location: all of
    /// it, or a folder shared with them.
    pub fn sees_any(&self, root_id: &str) -> bool {
        self.sees_root(root_id) || self.shared.iter().any(|g| g.root_id == root_id)
    }

    /// The innermost shared folder holding `path`.
    pub fn grant_at(&self, root_id: &str, path: &str) -> Option<&FolderGrant> {
        self.shared
            .iter()
            .filter(|g| g.root_id == root_id && within(path, &g.path))
            .max_by_key(|g| g.path.len())
    }

    /// Whether the storage location or a folder shared with the caller lets
    /// them do `need` at `path`. The role is checked separately.
    pub fn may(&self, root_id: &str, path: &str, need: Need) -> bool {
        if self.sees_root(root_id) {
            return true;
        }
        let Some(grant) = self.grant_at(root_id, path) else { return false };
        match need {
            Need::Read => matches!(grant.access, FolderAccess::View | FolderAccess::Edit),
            Need::Write => grant.access == FolderAccess::Edit,
            Need::Add => matches!(grant.access, FolderAccess::Add | FolderAccess::Edit),
            Need::Change => grant.access == FolderAccess::Edit && !path.eq_ignore_ascii_case(&grant.path),
        }
    }

    /// Like [`may`](Self::may), with the reason when not. What the caller
    /// cannot see at all looks missing.
    pub fn require_access(&self, root_id: &str, path: &str, need: Need) -> Result<(), ApiError> {
        if self.may(root_id, path, need) {
            return Ok(());
        }
        let Some(grant) = self.grant_at(root_id, path) else {
            return Err(NexusError::NotFound(if self.sees_any(root_id) { "Folder" } else { "Storage location" }.into()).into());
        };
        let why = match (grant.access, need) {
            (FolderAccess::Add, Need::Read) => "This folder only takes new files: its contents are not shown",
            (FolderAccess::Add, _) => "This folder only takes new files: nothing in it can be changed",
            (FolderAccess::View, _) => "This folder is shared with you to view only",
            (FolderAccess::Edit, _) => "A shared folder itself cannot be renamed, moved or deleted by the people it is shared with",
        };
        Err(NexusError::PermissionDenied(why.into()).into())
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
    if token.starts_with(crate::tokens::PREFIX) {
        return crate::tokens::authenticate(state, token).await;
    }
    if token_matches(token, &state.local_token) {
        let owner = state.db.first_owner().await?;
        return Ok(AuthUser {
            user_id: owner.as_ref().map(|u| u.id.clone()),
            username: owner.map(|u| u.username).unwrap_or_else(|| "local".into()),
            role: Role::Owner,
            local: true,
            device_id: None,
            allowed_roots: None,
            shared: Vec::new(),
            via_token: false,
        });
    }
    match state.db.session_user(&hash_token(token)).await? {
        Some(user) => AuthUser::person(user).with_shared(state).await,
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
            acting_roots: get(HDR_ACTING_ROOTS),
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
/// person's role on their node; the storage they see is what the device may
/// see here, narrowed to the person's own limits if they have any.
pub async fn authenticate_device(state: &AppState, signed: &SignedHeaders, via: &P2pPeer) -> Result<AuthUser> {
    let device = state.db.get_device(&signed.device_id).await.map_err(|_| NexusError::Unauthorized)?;
    let key = device.public_key.as_deref().filter(|_| !device.is_local).ok_or(NexusError::Unauthorized)?;
    if device.blocked_at.is_some() {
        return Err(NexusError::PermissionDenied("This device is blocked".into()));
    }
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
    let acting_roots = signed.request.acting_root_list();
    let mut user = AuthUser {
        user_id: None,
        username: if person.is_empty() { device.name.clone() } else { format!("{person} on {}", device.name) },
        role: device.role.least(acting),
        local: false,
        allowed_roots: narrow(device.allowed_root_list(), acting_roots.clone()),
        device_id: Some(device.id),
        shared: Vec::new(),
        via_token: false,
    }
    .with_shared(state)
    .await?;
    // A folder shared with the device is for the people using it who may
    // see that storage location on their own node.
    if let Some(theirs) = acting_roots {
        user.shared.retain(|g| theirs.contains(&g.root_id));
    }
    Ok(user)
}

/// Both limits at once: only locations allowed by each.
fn narrow(a: Option<Vec<String>>, b: Option<Vec<String>>) -> Option<Vec<String>> {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => Some(a.into_iter().filter(|r| b.contains(r)).collect()),
    }
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
            let user = authenticate(state, token).await?;
            if user.via_token && crate::tokens::forbidden_path(&original_path_and_query(parts)) {
                return Err(NexusError::PermissionDenied("Tokens for programs cannot manage people, tokens, pairing or sign-in".into()).into());
            }
            return Ok(user);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(path: &str, access: FolderAccess) -> FolderGrant {
        FolderGrant {
            folder_id: path.into(),
            root_id: "r".into(),
            path: path.into(),
            name: path.into(),
            access,
            created_by: "owner".into(),
            member: "user:u".into(),
        }
    }

    fn limited(shared: Vec<FolderGrant>) -> AuthUser {
        AuthUser {
            user_id: Some("u".into()),
            username: "anna".into(),
            role: Role::Editor,
            local: false,
            device_id: None,
            allowed_roots: Some(vec![]),
            shared,
            via_token: false,
        }
    }

    #[test]
    fn shared_folders_add_access_by_path() {
        let u = limited(vec![grant("Family", FolderAccess::View), grant("Family/Inbox", FolderAccess::Add), grant("Work", FolderAccess::Edit)]);
        assert!(u.may("r", "Family", Need::Read));
        assert!(u.may("r", "family/photos/a.jpg", Need::Read));
        assert!(!u.may("r", "Family/a.jpg", Need::Write));
        assert!(!u.may("r", "Familyx", Need::Read));
        assert!(!u.may("r", "", Need::Read));
        assert!(!u.may("other", "Family", Need::Read));
        // The innermost shared folder decides.
        assert!(!u.may("r", "Family/Inbox", Need::Read));
        assert!(u.may("r", "Family/Inbox", Need::Add));
        assert!(!u.may("r", "Family/Inbox/x.txt", Need::Write));
        // Edit: everything inside, but not the shared folder itself.
        assert!(u.may("r", "Work", Need::Write));
        assert!(u.may("r", "Work/a.txt", Need::Change));
        assert!(!u.may("r", "Work", Need::Change));
        assert!(!u.may("r", "work", Need::Change));
        assert!(u.sees_any("r") && !u.sees_any("other"));
    }

    #[test]
    fn whole_locations_still_count() {
        let mut u = limited(vec![grant("Family", FolderAccess::View)]);
        u.allowed_roots = None;
        assert!(u.may("r", "", Need::Change));
        assert!(u.may("elsewhere", "x", Need::Write));
    }
}
