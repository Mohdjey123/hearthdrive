//! The NEXUS virtual drive: every paired device's storage in File Explorer.
//!
//! On Windows the drive is a cloud sync root, the mechanism OneDrive uses
//! (the Cloud Files API): a folder, shown in Explorer's navigation pane,
//! whose contents are placeholders. Folders are listed when first opened
//! and files are downloaded from their device when first read; Explorer's
//! own "Always keep on this device" and "Free up space" work on them.
//!
//! Everything is read through the local node's own API, as its owner, so
//! the drive shows what the app's Files page shows. Phase 1 is read-only:
//! files are marked read-only, and deleting or renaming placeholders is
//! refused.

pub mod client;
pub mod tree;
#[cfg(windows)]
mod windows;

use client::NodeClient;
use nexus_common::events::EventBus;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Whether the drive is on (`1`/`0`).
pub const ENABLED_SETTING: &str = "vdrive.enabled";
/// The folder chosen for the drive (empty: the default).
pub const PATH_SETTING: &str = "vdrive.path";

/// What the drive needs from the node.
pub struct Context {
    pub client: NodeClient,
    pub events: EventBus,
    /// The node's data folder (the drive keeps its icon there).
    pub data_dir: PathBuf,
    /// This node's device id: one drive per node and Windows account.
    pub account: String,
    pub runtime: tokio::runtime::Handle,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Status {
    /// The drive can run here (Windows 10 1809 or later).
    pub supported: bool,
    pub enabled: bool,
    /// Registered and serving File Explorer right now.
    pub running: bool,
    /// The drive's folder.
    pub path: String,
    pub default_path: String,
    /// Why the drive is not running although it is on.
    pub error: Option<String>,
}

#[derive(Default)]
struct State {
    enabled: bool,
    path: Option<PathBuf>,
    error: Option<String>,
    #[cfg(windows)]
    provider: Option<windows::Provider>,
}

/// Turns the drive on and off. Cheap to share; does nothing until
/// [`VirtualDrive::attach`] gives it a node.
#[derive(Default)]
pub struct VirtualDrive {
    context: std::sync::OnceLock<Arc<Context>>,
    state: Mutex<State>,
    /// One change at a time.
    applying: tokio::sync::Mutex<()>,
}

/// The default folder: `%USERPROFILE%\NEXUS`.
pub fn default_path() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("NEXUS")
}

/// True when `a` is `b` or inside it (ignoring case, as Windows does).
fn within(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
        s.trim_end_matches('\\').to_string()
    };
    let (a, b) = (norm(a), norm(b));
    a == b || a.starts_with(&format!("{b}\\"))
}

/// Checks a folder chosen for the drive: an absolute path that is neither
/// inside nor around any of `avoid` (storage locations, the node's data):
/// the drive's downloaded files must never be indexed or shared again.
pub fn check_path(path: &Path, avoid: &[PathBuf]) -> Result<(), String> {
    let text = path.to_string_lossy();
    let absolute = path.is_absolute() && (text.len() > 3 || !text.ends_with(['\\', '/']));
    if !absolute || text.starts_with("\\\\") {
        return Err("Choose a folder on a drive of this computer, such as C:\\Users\\you\\NEXUS".into());
    }
    if path.parent().is_none() || path.parent() == Some(Path::new("")) {
        return Err("Choose a folder, not a whole drive".into());
    }
    for other in avoid {
        if within(path, other) || within(other, path) {
            return Err(format!(
                "The virtual drive cannot be in or around {}, which NEXUS already uses. Choose another folder.",
                other.display()
            ));
        }
    }
    Ok(())
}

impl VirtualDrive {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connects the drive to its node. Only the first call counts.
    pub fn attach(&self, context: Context) {
        let _ = self.context.set(Arc::new(context));
    }

    pub fn supported() -> bool {
        #[cfg(windows)]
        {
            windows::supported()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    pub fn status(&self) -> Status {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(windows)]
        let running = state.provider.is_some();
        #[cfg(not(windows))]
        let running = false;
        Status {
            supported: Self::supported(),
            enabled: state.enabled,
            running,
            path: state.path.clone().unwrap_or_else(default_path).display().to_string(),
            default_path: default_path().display().to_string(),
            error: state.error.clone(),
        }
    }

    /// Turns the drive on in `path` (or moves it there), or off. Turning it
    /// off, or moving it, removes it from File Explorer and deletes its
    /// placeholders and downloaded copies; files of your own put in the
    /// folder stay.
    pub async fn apply(&self, enabled: bool, path: Option<PathBuf>) -> Result<Status, String> {
        let _one = self.applying.lock().await;
        let path = path.unwrap_or_else(default_path);
        #[cfg(windows)]
        {
            let context = self.context.get().cloned().ok_or("The virtual drive is not ready yet")?;
            let previous = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let same = state.provider.as_ref().is_some_and(|p| within(p.path(), &path) && within(&path, p.path()));
                if enabled && same {
                    state.enabled = true;
                    state.error = None;
                    drop(state);
                    return Ok(self.status());
                }
                state.provider.take()
            };
            if enabled {
                let (context, path) = (context.clone(), path.clone());
                let checked = tokio::task::spawn_blocking(move || windows::check_folder(&context, &path))
                    .await
                    .map_err(|e| e.to_string())?;
                if let Err(e) = checked {
                    // Keep the drive where it was.
                    self.state.lock().unwrap_or_else(|e| e.into_inner()).provider = previous;
                    return Err(e);
                }
            }
            if let Some(previous) = previous {
                tokio::task::spawn_blocking(move || previous.stop(true)).await.map_err(|e| e.to_string())?;
            }
            let started = if enabled {
                let path = path.clone();
                Some(
                    tokio::task::spawn_blocking(move || windows::Provider::start(context, &path))
                        .await
                        .map_err(|e| e.to_string())?,
                )
            } else {
                // Off: make sure no registration is left behind (from a
                // previous run that stopped without cleaning up).
                let context = context.clone();
                let _ = tokio::task::spawn_blocking(move || windows::unregister_leftover(&context)).await;
                None
            };
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.enabled = enabled;
            state.path = Some(path);
            match started {
                Some(Ok(provider)) => {
                    state.provider = Some(provider);
                    state.error = None;
                }
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "the virtual drive could not start");
                    state.enabled = false;
                    state.error = Some(e.clone());
                    drop(state);
                    return Err(e);
                }
                None => state.error = None,
            }
        }
        #[cfg(not(windows))]
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if enabled {
                return Err("The virtual drive is only available on Windows".into());
            }
            state.enabled = false;
            state.path = Some(path);
        }
        Ok(self.status())
    }

    /// Records the saved choice without starting anything (when starting
    /// failed, so the app still shows the drive as on, with the error).
    pub fn remember(&self, enabled: bool, path: Option<PathBuf>, error: Option<String>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.enabled = enabled;
        state.path = path;
        state.error = error;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_chosen_folders() {
        let avoid = [PathBuf::from("D:\\Media"), PathBuf::from("C:\\Users\\me\\AppData\\Local\\NEXUS Cloud")];
        if cfg!(windows) {
            assert!(check_path(Path::new("C:\\Users\\me\\NEXUS"), &avoid).is_ok());
            assert!(check_path(Path::new("D:\\Other"), &avoid).is_ok());
            assert!(check_path(Path::new("D:\\Media\\NEXUS"), &avoid).is_err(), "inside a storage location");
            assert!(check_path(Path::new("d:\\media"), &avoid).is_err(), "case does not matter");
            assert!(check_path(Path::new("D:\\"), &avoid).is_err(), "a whole drive");
            assert!(check_path(Path::new("C:\\Users\\me\\AppData"), &avoid).is_err(), "around the data folder");
            assert!(check_path(Path::new("\\\\server\\share\\x"), &avoid).is_err(), "network shares");
        }
        assert!(check_path(Path::new("relative\\path"), &avoid).is_err());
    }

    #[test]
    fn within_matches_whole_names_only() {
        assert!(within(Path::new("C:\\A\\b"), Path::new("C:\\a")));
        assert!(within(Path::new("C:\\A"), Path::new("c:\\a\\")));
        assert!(!within(Path::new("C:\\AB"), Path::new("C:\\A")));
    }

    #[test]
    fn default_path_is_in_the_profile() {
        assert!(default_path().ends_with("NEXUS"));
    }

    #[tokio::test]
    async fn starts_off() {
        let drive = VirtualDrive::new();
        let status = drive.status();
        assert!(!status.enabled && !status.running);
        assert_eq!(status.path, status.default_path);
        if !cfg!(windows) {
            assert!(drive.apply(true, None).await.is_err());
        }
    }
}
