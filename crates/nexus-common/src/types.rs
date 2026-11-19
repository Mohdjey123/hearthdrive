use serde::{Deserialize, Serialize};

/// Where an indexed item can currently be reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum Availability {
    Online,
    Offline,
    Synchronizing,
    Downloading,
    Uploading,
    Missing,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum RootType {
    /// Hearthdrive created and manages the directory.
    Managed,
    /// A folder the user already had.
    Existing,
    /// A removable drive.
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Administrator,
    Editor,
    Contributor,
    Viewer,
    Restricted,
}

impl Role {
    pub const ALL: [Role; 6] =
        [Role::Owner, Role::Administrator, Role::Editor, Role::Contributor, Role::Viewer, Role::Restricted];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Administrator => "administrator",
            Role::Editor => "editor",
            Role::Contributor => "contributor",
            Role::Viewer => "viewer",
            Role::Restricted => "restricted",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// The less privileged of two roles.
    pub fn least(self, other: Role) -> Role {
        let rank = |r: Role| Self::ALL.iter().position(|x| *x == r).unwrap_or(usize::MAX);
        if rank(self) >= rank(other) { self } else { other }
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub device_type: String,
    /// Base64url Ed25519 public key; every paired device has one.
    pub public_key: Option<String>,
    /// Last address (`host:port`) the device answered on.
    pub network_address: Option<String>,
    pub is_local: bool,
    pub last_seen: Option<i64>,
    /// `online` | `offline`.
    pub status: String,
    pub created_at: i64,
    /// What this device may do on this node.
    pub role: Role,
    /// JSON array of ways to reach the device (`relay:<url>` or `ip:port`).
    #[serde(skip)]
    pub addresses: String,
    pub latency_ms: Option<i64>,
    pub paired_at: Option<i64>,
    pub version: Option<String>,
    /// JSON array of the storage root ids this device may see; `None` = all.
    #[serde(skip)]
    pub allowed_roots: Option<String>,
    /// The paired device that introduced this one, if it was introduced
    /// rather than paired directly.
    pub introduced_by: Option<String>,
    /// When the device was blocked: its requests are refused and nothing is
    /// sent to it.
    pub blocked_at: Option<i64>,
    /// JSON array of its network cards' MAC addresses (Wake-on-LAN).
    #[serde(skip)]
    pub mac_addresses: Option<String>,
    /// JSON array of the broadcast addresses of its local networks.
    #[serde(skip)]
    pub broadcast: Option<String>,
}

impl Device {
    pub fn address_list(&self) -> Vec<String> {
        serde_json::from_str(&self.addresses).unwrap_or_default()
    }

    /// The storage locations this device is limited to, if it is limited.
    pub fn allowed_root_list(&self) -> Option<Vec<String>> {
        parse_root_list(self.allowed_roots.as_deref())
    }
}

/// A device asking to join this node's cloud.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PairingRequest {
    pub id: String,
    pub session_id: String,
    pub device_id: String,
    pub name: String,
    pub device_type: String,
    pub public_key: String,
    #[serde(skip)]
    pub addresses: String,
    pub version: Option<String>,
    pub remote_ip: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

impl PairingRequest {
    pub fn address_list(&self) -> Vec<String> {
        serde_json::from_str(&self.addresses).unwrap_or_default()
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PairingSession {
    pub id: String,
    pub expires_at: i64,
    pub closed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StorageRoot {
    pub id: String,
    pub device_id: String,
    pub name: String,
    pub absolute_path: String,
    pub root_type: RootType,
    pub read_only: bool,
    pub enabled: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ScanState {
    pub root_id: String,
    /// idle | scanning | error | offline
    pub status: String,
    pub files_seen: i64,
    pub bytes_seen: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FileEntry {
    pub id: String,
    pub root_id: String,
    pub relative_path: String,
    pub parent_path: String,
    pub filename: String,
    pub is_dir: bool,
    pub extension: Option<String>,
    pub mime_type: Option<String>,
    pub size: i64,
    pub created_at: Option<i64>,
    pub modified_at: Option<i64>,
    pub content_hash: Option<String>,
    pub indexed_at: i64,
    pub availability: Availability,
}

/// Filesystem metadata ready to be written to the index.
#[derive(Debug, Clone)]
pub struct NewEntry {
    pub relative_path: String,
    pub parent_path: String,
    pub filename: String,
    pub is_dir: bool,
    pub extension: Option<String>,
    pub mime_type: Option<String>,
    pub size: i64,
    pub created_at: Option<i64>,
    pub modified_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub role: Role,
    pub created_at: i64,
    /// JSON array of the storage root ids this person may see; `None` = all.
    /// Sent as a list (or `null`).
    #[serde(serialize_with = "root_list")]
    pub allowed_roots: Option<String>,
}

fn root_list<S: serde::Serializer>(raw: &Option<String>, s: S) -> std::result::Result<S::Ok, S::Error> {
    use serde::Serialize;
    parse_root_list(raw.as_deref()).serialize(s)
}

fn parse_root_list(raw: Option<&str>) -> Option<Vec<String>> {
    raw.map(|json| serde_json::from_str(json).unwrap_or_default())
}

impl User {
    /// Owners and administrators manage everything, so they are never
    /// limited; everyone else may be limited to some storage locations.
    pub fn limited_roots(&self) -> Option<Vec<String>> {
        if matches!(self.role, Role::Owner | Role::Administrator) {
            None
        } else {
            parse_root_list(self.allowed_roots.as_deref())
        }
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DeletedFile {
    pub id: String,
    pub root_id: String,
    pub original_path: String,
    pub filename: String,
    pub is_dir: bool,
    pub size: i64,
    pub trash_name: String,
    pub deleted_at: i64,
    pub deleted_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ActivityEntry {
    pub id: String,
    pub action: String,
    pub resource: Option<String>,
    pub username: Option<String>,
    pub timestamp: i64,
}
