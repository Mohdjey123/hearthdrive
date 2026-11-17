//! Shared knowledge between the background service and the desktop shell:
//! where node data lives and how to find the running service.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Written by the service into the data directory once it is listening.
/// The data directory is inside the user's profile, so only that user (and
/// administrators) can read the local token.
pub const RUNTIME_FILE: &str = "runtime.json";
pub const DEFAULT_PORT: u16 = 47100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub pid: u32,
    pub port: u16,
    pub base_url: String,
    /// Grants owner access to processes running as this Windows user.
    pub local_token: String,
    pub started_at: i64,
}

/// `%LOCALAPPDATA%\NEXUS Cloud` on Windows, overridable with `NEXUS_DATA_DIR`.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NEXUS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("NEXUS Cloud");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("nexus-cloud");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local").join("share").join("nexus-cloud");
    }
    PathBuf::from("nexus-data")
}
