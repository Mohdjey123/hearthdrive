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
    /// The service's program file and when it was written, so an app that
    /// was updated can tell that an older service is still running.
    #[serde(default)]
    pub exe: Option<String>,
    #[serde(default)]
    pub exe_modified: Option<i64>,
}

/// When a program file was last written, in milliseconds.
pub fn program_stamp(path: &std::path::Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

/// `%LOCALAPPDATA%\HearthDrive` on Windows, overridable with `NEXUS_DATA_DIR`.
/// A computer that ran the app under its earlier name, NEXUS Cloud, keeps
/// its data in `%LOCALAPPDATA%\NEXUS Cloud`: the device's identity, the
/// database and everything else stay where they are.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NEXUS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            let earlier = local.join("NEXUS Cloud");
            if earlier.join("identity.key").is_file() {
                return earlier;
            }
            return local.join("HearthDrive");
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
