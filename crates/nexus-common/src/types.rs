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
    /// NEXUS created and manages the directory.
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
}

impl Device {
    pub fn address_list(&self) -> Vec<String> {
        serde_json::from_str(&self.addresses).unwrap_or_default()
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
