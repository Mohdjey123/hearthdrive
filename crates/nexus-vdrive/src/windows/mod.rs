//! The drive on Windows: a Cloud Files API sync provider.
//!
//! Windows calls back into this process when Explorer or any app opens a
//! folder that was never listed (`FETCH_PLACEHOLDERS`) or reads a file that
//! is not on this computer yet (`FETCH_DATA`). Callbacks only copy what they
//! need and hand the work to the node's async runtime, which answers Windows
//! with `CfExecute` when the device has replied.
//!
//! Changes made here go back: deletes and renames are carried out on the
//! device before Windows is told they may happen, and edited or new files
//! and folders are uploaded once they are closed.

mod ops;
mod register;

use crate::Context;
use crate::client::{ClientError, EntryInfo};
use crate::tree::{self, Change, Child, Identity, Local, transfer_range};
use futures_util::StreamExt;
use nexus_common::events::Event;
use ops::Attrs;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use windows::Storage::Provider::StorageProviderSyncRootManager;
use windows::Win32::Foundation::{
    NTSTATUS, STATUS_CLOUD_FILE_ACCESS_DENIED, STATUS_CLOUD_FILE_INVALID_REQUEST, STATUS_CLOUD_FILE_NETWORK_UNAVAILABLE,
    STATUS_CLOUD_FILE_NOT_IN_SYNC, STATUS_CLOUD_FILE_NOT_SUPPORTED, STATUS_CLOUD_FILE_READ_ONLY_VOLUME,
    STATUS_CLOUD_FILE_UNSUCCESSFUL, STATUS_SUCCESS,
};
use windows::Win32::Storage::CloudFilters::*;
use windows::core::PCWSTR;

/// Bytes fetched at least per request, so reading a file in small pieces
/// does not cost a network round trip per piece.
const READ_AHEAD: u64 = 16 * 1024 * 1024;
/// Bytes handed to Windows per data transfer (a multiple of 4 KiB).
const CHUNK: usize = 1024 * 1024;
/// How often a device's storage is checked for changes at most.
const CHECK_EVERY: Duration = Duration::from_secs(60);
/// Pin changes arriving together are handled together.
const PIN_SETTLE: Duration = Duration::from_millis(400);
/// A file still being written is tried again after this long.
const RETRY_UPLOAD: Duration = Duration::from_secs(10);
/// How long a device's change feed waits for a change, in seconds.
const FEED_WAIT: u64 = 15;
/// How often the offline cache is looked at.
const CACHE_EVERY: Duration = Duration::from_secs(10 * 60);
/// Opened this often, a file counts as often used.
const FREQUENT_USES: u32 = 3;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How often and when a downloaded file was last opened.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct Usage {
    count: u32,
    last: i64,
}

impl Usage {
    fn frequent(&self, now: i64) -> bool {
        self.count >= FREQUENT_USES && now - self.last < 60 * DAY_MS
    }
}

pub fn supported() -> bool {
    StorageProviderSyncRootManager::IsSupported().unwrap_or(true)
}

/// State shared by the callbacks and the background tasks.
struct Shared {
    ctx: Arc<Context>,
    root: PathBuf,
    /// Folders already listed, by their path here: what each shows
    /// (`None` for the drive's own folder).
    listed: Mutex<HashMap<PathBuf, Option<Identity>>>,
    /// Downloads in progress, by transfer key.
    transfers: Mutex<HashMap<i64, Vec<(u64, tokio::task::AbortHandle)>>>,
    next_transfer: std::sync::atomic::AtomicU64,
    /// What each storage location looked like at the last check.
    fingerprints: Mutex<HashMap<String, String>>,
    checked: Mutex<HashMap<String, Instant>>,
    /// Devices last shown as offline (true) or online (false).
    offline: Mutex<HashMap<String, bool>>,
    /// Uses of downloaded files (the offline cache keeps often used ones).
    usage: Mutex<HashMap<PathBuf, Usage>>,
    cache: Arc<crate::Cache>,
    cleared: AtomicU64,
    /// How far each device's change feed was read.
    cursors: Mutex<HashMap<String, String>>,
    /// Devices whose change feed is followed, with the task doing it.
    following: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    /// Paths to look at again for changes made here (busy files).
    local: tokio::sync::mpsc::UnboundedSender<PathBuf>,
}

/// The drive serving Explorer right now (callbacks have no other way in).
static ACTIVE: RwLock<Option<Arc<Shared>>> = RwLock::new(None);

fn active() -> Option<Arc<Shared>> {
    ACTIVE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

/// A running drive.
pub struct Provider {
    shared: Arc<Shared>,
    id: String,
    key: CF_CONNECTION_KEY,
    _watcher: Option<notify::RecommendedWatcher>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

static CALLBACKS: [CF_CALLBACK_REGISTRATION; 6] = [
    CF_CALLBACK_REGISTRATION { Type: CF_CALLBACK_TYPE_FETCH_PLACEHOLDERS, Callback: Some(on_fetch_placeholders) },
    CF_CALLBACK_REGISTRATION { Type: CF_CALLBACK_TYPE_FETCH_DATA, Callback: Some(on_fetch_data) },
    CF_CALLBACK_REGISTRATION { Type: CF_CALLBACK_TYPE_CANCEL_FETCH_DATA, Callback: Some(on_cancel_fetch_data) },
    CF_CALLBACK_REGISTRATION { Type: CF_CALLBACK_TYPE_NOTIFY_DELETE, Callback: Some(on_delete) },
    CF_CALLBACK_REGISTRATION { Type: CF_CALLBACK_TYPE_NOTIFY_RENAME, Callback: Some(on_rename) },
    CF_CALLBACK_REGISTRATION { Type: CF_CALLBACK_TYPE_NONE, Callback: None },
];

/// Refuses a folder that holds anything and is not already the drive (it
/// would mix your files with placeholders).
pub fn check_folder(ctx: &Context, path: &Path) -> Result<(), String> {
    let empty = std::fs::read_dir(path).map(|mut d| d.next().is_none()).unwrap_or(true);
    if empty {
        return Ok(());
    }
    let id = register::sync_root_id(&ctx.account)?;
    let ours = register::registered_path(&id).is_some_and(|p| {
        std::fs::canonicalize(path).ok().zip(std::fs::canonicalize(&p).ok()).is_some_and(|(a, b)| a == b)
    });
    if ours {
        Ok(())
    } else {
        Err(format!("{} is not empty. Choose an empty or new folder for the virtual drive.", path.display()))
    }
}

/// Removes a registration left by an earlier run (the drive is off).
pub fn unregister_leftover(ctx: &Context) {
    let Ok(id) = register::sync_root_id(&ctx.account) else { return };
    if let Some(path) = register::registered_path(&id) {
        tracing::info!(path = %path.display(), "removing the virtual drive left from an earlier run");
        let _ = ops::remove_contents(&path);
        let _ = register::unregister(&id);
        let _ = std::fs::remove_dir(&path);
    }
}

impl Provider {
    pub fn path(&self) -> &Path {
        &self.shared.root
    }

    /// Registers `path` as the drive and starts serving it.
    pub fn start(ctx: Arc<Context>, path: &Path, cache: Arc<crate::Cache>) -> Result<Self, String> {
        if !supported() {
            return Err("This version of Windows does not support cloud files (Windows 10 1809 or later is needed)".into());
        }
        let id = register::sync_root_id(&ctx.account)?;
        std::fs::create_dir_all(path).map_err(|e| format!("Creating {}: {e}", path.display()))?;
        let root = std::fs::canonicalize(path).map_err(|e| format!("Opening {}: {e}", path.display()))?;
        let root = PathBuf::from(root.to_string_lossy().trim_start_matches(r"\\?\"));

        // The drive moved: the old folder is no longer it.
        match register::registered_path(&id) {
            Some(old) if !same_path(&old, &root) => {
                tracing::info!(old = %old.display(), "moving the virtual drive");
                let _ = ops::remove_contents(&old);
                let _ = register::unregister(&id);
                let _ = std::fs::remove_dir(&old);
            }
            Some(_) => {}
            None => check_folder(&ctx, &root)?,
        }
        register::register(&id, &root, &ctx.data_dir)?;

        let (local_tx, local_rx) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            ctx: ctx.clone(),
            root: root.clone(),
            listed: Mutex::new(HashMap::new()),
            transfers: Mutex::new(HashMap::new()),
            next_transfer: Default::default(),
            fingerprints: Mutex::new(HashMap::new()),
            checked: Mutex::new(HashMap::new()),
            offline: Mutex::new(HashMap::new()),
            usage: Mutex::new(load_usage(&ctx.data_dir)),
            cache,
            cleared: AtomicU64::new(0),
            cursors: Mutex::new(HashMap::new()),
            following: Mutex::new(HashMap::new()),
            local: local_tx.clone(),
        });
        *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = Some(shared.clone());

        let wide = ops::wide(&root);
        let key = match unsafe {
            CfConnectSyncRoot(
                PCWSTR(wide.as_ptr()),
                CALLBACKS.as_ptr(),
                None,
                CF_CONNECT_FLAG_REQUIRE_PROCESS_INFO | CF_CONNECT_FLAG_REQUIRE_FULL_FILE_PATH,
            )
        } {
            Ok(key) => key,
            Err(e) => {
                *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = None;
                let _ = register::unregister(&id);
                return Err(format!("Connecting the virtual drive: {}", e.message()));
            }
        };
        tracing::info!(path = %root.display(), "virtual drive running");

        let watcher = match watch_pins(&root, local_tx) {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::warn!(error = %e, "cannot watch the virtual drive; changes made in it and Always keep / Free up space will not act");
                None
            }
        };
        let mut tasks = Vec::new();
        let rt = &ctx.runtime;
        tasks.push(rt.spawn(follow_events(shared.clone())));
        tasks.push(rt.spawn(handle_pins(shared.clone(), local_rx)));
        tasks.push(rt.spawn(keep_cache(shared.clone())));
        Ok(Self { shared, id, key, _watcher: watcher, tasks })
    }

    /// Stops serving the drive and removes it from Explorer. With `remove`,
    /// its placeholders and downloaded copies are deleted too (files of
    /// your own stay).
    pub fn stop(self, remove: bool) {
        for task in &self.tasks {
            task.abort();
        }
        for (_, task) in self.shared.following.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            task.abort();
        }
        save_usage(&self.shared);
        if remove {
            match ops::remove_contents(&self.shared.root) {
                Ok(true) => {}
                Ok(false) => tracing::info!("files of your own stay in the virtual drive's folder"),
                Err(e) => tracing::warn!(error = %e, "could not remove all of the virtual drive"),
            }
        }
        unsafe {
            let _ = CfDisconnectSyncRoot(self.key);
        }
        *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = None;
        if let Err(e) = register::unregister(&self.id) {
            tracing::warn!(error = %e, "could not unregister the virtual drive");
        }
        if remove {
            let _ = std::fs::remove_dir(&self.shared.root);
        }
        tracing::info!("virtual drive stopped");
    }
}

// ---------------------------------------------------------------------------
// Callbacks

/// What a callback needs to answer Windows later.
struct Request {
    key: CF_CONNECTION_KEY,
    transfer: i64,
    request: i64,
    path: PathBuf,
    identity: Vec<u8>,
    file_size: i64,
    pid: u32,
}

unsafe fn text(p: PCWSTR) -> String {
    if p.is_null() { String::new() } else { unsafe { p.to_string().unwrap_or_default() } }
}

unsafe fn request_of(info: *const CF_CALLBACK_INFO) -> Request {
    let info = unsafe { &*info };
    let identity = if info.FileIdentity.is_null() || info.FileIdentityLength == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(info.FileIdentity.cast::<u8>(), info.FileIdentityLength as usize).to_vec() }
    };
    let path = unsafe { format!("{}{}", text(info.VolumeDosName), text(info.NormalizedPath)) };
    let pid = if info.ProcessInfo.is_null() { 0 } else { unsafe { (*info.ProcessInfo).ProcessId } };
    Request {
        key: info.ConnectionKey,
        transfer: info.TransferKey,
        request: info.RequestKey,
        path: PathBuf::from(path),
        identity,
        file_size: info.FileSize,
        pid,
    }
}

fn op_info(r: &Request, kind: CF_OPERATION_TYPE) -> CF_OPERATION_INFO {
    CF_OPERATION_INFO {
        StructSize: std::mem::size_of::<CF_OPERATION_INFO>() as u32,
        Type: kind,
        ConnectionKey: r.key,
        TransferKey: r.transfer,
        CorrelationVector: std::ptr::null(),
        SyncStatus: std::ptr::null(),
        RequestKey: r.request,
    }
}

fn param_size<T>() -> u32 {
    (std::mem::offset_of!(CF_OPERATION_PARAMETERS, Anonymous) + std::mem::size_of::<T>()) as u32
}

/// Answers a folder listing with `children`, or fails it with `status`.
fn transfer_placeholders(r: &Request, children: &[Child], status: NTSTATUS) -> windows::core::Result<()> {
    let mut set = ops::create_infos(children);
    let count = set.infos.len() as u32;
    let info = op_info(r, CF_OPERATION_TYPE_TRANSFER_PLACEHOLDERS);
    let mut params = CF_OPERATION_PARAMETERS {
        ParamSize: param_size::<CF_OPERATION_PARAMETERS_0_4>(),
        Anonymous: CF_OPERATION_PARAMETERS_0 {
            TransferPlaceholders: CF_OPERATION_PARAMETERS_0_4 {
                Flags: if status == STATUS_SUCCESS {
                    CF_OPERATION_TRANSFER_PLACEHOLDERS_FLAG_DISABLE_ON_DEMAND_POPULATION
                } else {
                    CF_OPERATION_TRANSFER_PLACEHOLDERS_FLAG_NONE
                },
                CompletionStatus: status,
                PlaceholderTotalCount: count as i64,
                PlaceholderArray: if count == 0 { std::ptr::null_mut() } else { set.infos.as_mut_ptr() },
                PlaceholderCount: count,
                EntriesProcessed: 0,
            },
        },
    };
    unsafe { CfExecute(&info, &mut params) }
}

/// Hands Windows a piece of a file (or fails the range with `status`).
fn transfer_data(r: &Request, offset: u64, data: &[u8], len: u64, status: NTSTATUS) -> windows::core::Result<()> {
    let info = op_info(r, CF_OPERATION_TYPE_TRANSFER_DATA);
    let mut params = CF_OPERATION_PARAMETERS {
        ParamSize: param_size::<CF_OPERATION_PARAMETERS_0_0>(),
        Anonymous: CF_OPERATION_PARAMETERS_0 {
            TransferData: CF_OPERATION_PARAMETERS_0_0 {
                Flags: CF_OPERATION_TRANSFER_DATA_FLAG_NONE,
                CompletionStatus: status,
                Buffer: if data.is_empty() { std::ptr::null() } else { data.as_ptr().cast() },
                Offset: offset as i64,
                Length: len as i64,
            },
        },
    };
    unsafe { CfExecute(&info, &mut params) }
}

fn ack(r: &Request, kind: CF_OPERATION_TYPE, status: NTSTATUS) {
    let info = op_info(r, kind);
    let mut params = if kind == CF_OPERATION_TYPE_ACK_DELETE {
        CF_OPERATION_PARAMETERS {
            ParamSize: param_size::<CF_OPERATION_PARAMETERS_0_7>(),
            Anonymous: CF_OPERATION_PARAMETERS_0 {
                AckDelete: CF_OPERATION_PARAMETERS_0_7 { Flags: CF_OPERATION_ACK_DELETE_FLAG_NONE, CompletionStatus: status },
            },
        }
    } else {
        CF_OPERATION_PARAMETERS {
            ParamSize: param_size::<CF_OPERATION_PARAMETERS_0_6>(),
            Anonymous: CF_OPERATION_PARAMETERS_0 {
                AckRename: CF_OPERATION_PARAMETERS_0_6 { Flags: CF_OPERATION_ACK_RENAME_FLAG_NONE, CompletionStatus: status },
            },
        }
    };
    if let Err(e) = unsafe { CfExecute(&info, &mut params) } {
        tracing::debug!(error = %e, "could not answer a delete or rename");
    }
}

fn status_of(e: &ClientError) -> NTSTATUS {
    match e {
        ClientError::Offline(_) => STATUS_CLOUD_FILE_NETWORK_UNAVAILABLE,
        ClientError::NotFound(_) => STATUS_CLOUD_FILE_NOT_IN_SYNC,
        ClientError::Other(_) => STATUS_CLOUD_FILE_UNSUCCESSFUL,
    }
}

unsafe extern "system" fn on_fetch_placeholders(info: *const CF_CALLBACK_INFO, _params: *const CF_CALLBACK_PARAMETERS) {
    let r = unsafe { request_of(info) };
    let Some(shared) = active() else {
        let _ = transfer_placeholders(&r, &[], STATUS_CLOUD_FILE_UNSUCCESSFUL);
        return;
    };
    let rt = shared.ctx.runtime.clone();
    rt.spawn(async move {
        let identity = if same_path(&r.path, &shared.root) {
            None
        } else {
            match Identity::decode(&r.identity) {
                Some(id) => Some(id),
                None => {
                    let _ = transfer_placeholders(&r, &[], STATUS_CLOUD_FILE_INVALID_REQUEST);
                    return;
                }
            }
        };
        match shared.ctx.client.children(identity.as_ref()).await {
            Ok(children) => {
                if let Err(e) = transfer_placeholders(&r, &children, STATUS_SUCCESS) {
                    // Usually some names already exist (created by a refresh).
                    tracing::debug!(path = %r.path.display(), error = %e, "folder listing partly applied");
                }
                shared.listed.lock().unwrap_or_else(|e| e.into_inner()).insert(r.path.clone(), identity);
                tracing::debug!(path = %r.path.display(), items = children.len(), "listed folder");
            }
            Err(e) => {
                tracing::info!(path = %r.path.display(), error = %e, "could not list a folder of the virtual drive");
                let _ = transfer_placeholders(&r, &[], status_of(&e));
            }
        }
    });
}

unsafe extern "system" fn on_fetch_data(info: *const CF_CALLBACK_INFO, params: *const CF_CALLBACK_PARAMETERS) {
    let r = unsafe { request_of(info) };
    let fetch = unsafe { (*params).Anonymous.FetchData };
    let (offset, len) = (fetch.RequiredFileOffset.max(0) as u64, fetch.RequiredLength.max(0) as u64);
    let Some(shared) = active() else {
        let _ = transfer_data(&r, offset, &[], len, STATUS_CLOUD_FILE_UNSUCCESSFUL);
        return;
    };
    if offset == 0 {
        let mut usage = shared.usage.lock().unwrap_or_else(|e| e.into_inner());
        let u = usage.entry(r.path.clone()).or_default();
        u.count = u.count.saturating_add(1);
        u.last = nexus_common::time::now_ms();
    }
    let rt = shared.ctx.runtime.clone();
    let transfer = r.transfer;
    let me = shared.next_transfer.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let task_shared = shared.clone();
    let handle = rt.spawn(async move {
        fetch_data(&task_shared, &r, offset, len).await;
        let mut transfers = task_shared.transfers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = transfers.get_mut(&r.transfer) {
            list.retain(|(id, _)| *id != me);
            if list.is_empty() {
                transfers.remove(&r.transfer);
            }
        }
    });
    shared.transfers.lock().unwrap_or_else(|e| e.into_inner()).entry(transfer).or_default().push((me, handle.abort_handle()));
}

unsafe extern "system" fn on_cancel_fetch_data(info: *const CF_CALLBACK_INFO, _params: *const CF_CALLBACK_PARAMETERS) {
    let r = unsafe { request_of(info) };
    if let Some(shared) = active()
        && let Some(list) = shared.transfers.lock().unwrap_or_else(|e| e.into_inner()).remove(&r.transfer)
    {
        tracing::debug!(path = %r.path.display(), "download cancelled");
        for (_, task) in list {
            task.abort();
        }
    }
}

/// The answer for a change the device refused or could not make.
fn write_status(e: &ClientError) -> NTSTATUS {
    match e {
        ClientError::Offline(_) => STATUS_CLOUD_FILE_NETWORK_UNAVAILABLE,
        ClientError::NotFound(_) => STATUS_CLOUD_FILE_NOT_IN_SYNC,
        ClientError::Other(_) => STATUS_CLOUD_FILE_ACCESS_DENIED,
    }
}

/// Deleting a file or folder moves it to its device's trash first. Devices,
/// storage locations and the drive's sections cannot be deleted here.
unsafe extern "system" fn on_delete(info: *const CF_CALLBACK_INFO, _params: *const CF_CALLBACK_PARAMETERS) {
    let r = unsafe { request_of(info) };
    if r.pid == std::process::id() {
        ack(&r, CF_OPERATION_TYPE_ACK_DELETE, STATUS_SUCCESS);
        return;
    }
    let Some(shared) = active() else {
        ack(&r, CF_OPERATION_TYPE_ACK_DELETE, STATUS_CLOUD_FILE_UNSUCCESSFUL);
        return;
    };
    let Some(Identity::Entry { dev, id, .. }) = Identity::decode(&r.identity) else {
        tracing::info!(path = %r.path.display(), "refused deleting a device, storage location or section");
        ack(&r, CF_OPERATION_TYPE_ACK_DELETE, STATUS_CLOUD_FILE_READ_ONLY_VOLUME);
        return;
    };
    let rt = shared.ctx.runtime.clone();
    rt.spawn(async move {
        let status = delete_on_device(&shared, &dev, &id, &r.path).await;
        ack(&r, CF_OPERATION_TYPE_ACK_DELETE, status);
    });
}

async fn delete_on_device(shared: &Arc<Shared>, dev: &str, id: &str, path: &Path) -> NTSTATUS {
    match shared.ctx.client.delete(dev, id).await {
        Ok(()) | Err(ClientError::NotFound(_)) => {
            tracing::info!(path = %path.display(), "moved to its device's trash");
            forget(shared, path);
            STATUS_SUCCESS
        }
        Err(e) => {
            tracing::info!(path = %path.display(), error = %e, "the device did not delete it");
            write_status(&e)
        }
    }
}

/// Renaming or moving inside one storage location is done on the device
/// first. Moving out of the drive (to the Recycle Bin) deletes it there.
/// Moving to another location or device is refused: copy instead.
unsafe extern "system" fn on_rename(info: *const CF_CALLBACK_INFO, params: *const CF_CALLBACK_PARAMETERS) {
    let r = unsafe { request_of(info) };
    if r.pid == std::process::id() {
        ack(&r, CF_OPERATION_TYPE_ACK_RENAME, STATUS_SUCCESS);
        return;
    }
    let rename = unsafe { (*params).Anonymous.Rename };
    let target_inside = rename.Flags.0 & CF_CALLBACK_RENAME_FLAG_TARGET_IN_SCOPE.0 != 0;
    let target = unsafe { PathBuf::from(format!("{}{}", text((*info).VolumeDosName), text(rename.TargetPath))) };
    let Some(shared) = active() else {
        ack(&r, CF_OPERATION_TYPE_ACK_RENAME, STATUS_CLOUD_FILE_UNSUCCESSFUL);
        return;
    };
    let Some(Identity::Entry { dev, root, id, .. }) = Identity::decode(&r.identity) else {
        tracing::info!(path = %r.path.display(), "refused renaming a device, storage location or section");
        ack(&r, CF_OPERATION_TYPE_ACK_RENAME, STATUS_CLOUD_FILE_READ_ONLY_VOLUME);
        return;
    };
    let rt = shared.ctx.runtime.clone();
    rt.spawn(async move {
        if !target_inside {
            let status = delete_on_device(&shared, &dev, &id, &r.path).await;
            ack(&r, CF_OPERATION_TYPE_ACK_RENAME, status);
            return;
        }
        let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
        let parent_identity = tokio::task::spawn_blocking(move || ops::identity(&parent)).await.ok().flatten();
        let parent_path = match parent_identity {
            Some(Identity::Root { dev: d, root: rt }) if d == dev && rt == root => String::new(),
            Some(Identity::Entry { dev: d, root: rt, path, dir: true, .. }) if d == dev && rt == root => path,
            _ => {
                tracing::info!(to = %target.display(), "refused moving to another storage location or device");
                ack(&r, CF_OPERATION_TYPE_ACK_RENAME, STATUS_CLOUD_FILE_NOT_SUPPORTED);
                return;
            }
        };
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match shared.ctx.client.rename(&dev, &id, &name, &parent_path).await {
            Ok(entry) => {
                ack(&r, CF_OPERATION_TYPE_ACK_RENAME, STATUS_SUCCESS);
                tracing::info!(from = %r.path.display(), to = %target.display(), "renamed on its device");
                moved(&shared, &r.path, &target);
                // Windows finishes the rename after the answer.
                tokio::time::sleep(Duration::from_millis(500)).await;
                let task_shared = shared.clone();
                let _ = tokio::task::spawn_blocking(move || fix_identities(&task_shared, &target, &entry.relative_path)).await;
            }
            Err(e) => {
                tracing::info!(path = %r.path.display(), error = %e, "the device did not rename it");
                ack(&r, CF_OPERATION_TYPE_ACK_RENAME, write_status(&e));
            }
        }
    });
}

/// After a rename or move, gives the item (and what is in it) identities
/// with their new paths, so the next refresh keeps them.
fn fix_identities(shared: &Shared, path: &Path, new_rel: &str) {
    let Some(old) = ops::identity(path) else { return };
    let Identity::Entry { path: old_rel, .. } = &old else { return };
    let old_rel = old_rel.clone();
    let renamed = old.moved_to(new_rel);
    if let Err(e) = ops::set_identity(path, &renamed) {
        tracing::debug!(path = %path.display(), error = %e, "identity not updated");
    }
    if old.is_dir() {
        if let Some(entry) = shared.listed.lock().unwrap_or_else(|e| e.into_inner()).get_mut(path) {
            *entry = Some(renamed);
        }
        fn walk(shared: &Shared, dir: &Path, old: &str, new: &str, depth: usize) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let child = entry.path();
                let Some(identity) = ops::identity(&child) else { continue };
                let Identity::Entry { path: rel, .. } = &identity else { continue };
                let Some(rest) = rel.strip_prefix(old) else { continue };
                let updated = identity.moved_to(&format!("{new}{rest}"));
                let _ = ops::set_identity(&child, &updated);
                if updated.is_dir() {
                    if let Some(entry) = shared.listed.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&child) {
                        *entry = Some(updated);
                    }
                    let unlisted = std::fs::metadata(&child).map(|m| Attrs::of(&m).unlisted()).unwrap_or(true);
                    if !unlisted && depth < 64 {
                        walk(shared, &child, old, new, depth + 1);
                    }
                }
            }
        }
        walk(shared, path, &old_rel, new_rel, 0);
    }
}

/// Streams the requested part of a file from its device into the
/// placeholder. Any failure fails the rest of the range, so an app sees an
/// error, never an empty or partly wrong file.
async fn fetch_data(shared: &Arc<Shared>, r: &Request, offset: u64, required: u64) {
    let file_size = r.file_size.max(0) as u64;
    let (start, len) = transfer_range(offset, required, file_size, READ_AHEAD);
    // Fails what is left of the request, from `at` on.
    let fail = |at: u64, status: NTSTATUS| {
        let end = (start + len).max(offset + required);
        let _ = transfer_data(r, at, &[], end.saturating_sub(at), status);
    };
    let Some(Identity::Entry { dev, id, size, .. }) = Identity::decode(&r.identity) else {
        fail(start, STATUS_CLOUD_FILE_INVALID_REQUEST);
        return;
    };
    if size != file_size {
        tracing::info!(path = %r.path.display(), "file changed since it was listed; refreshing");
        fail(start, STATUS_CLOUD_FILE_NOT_IN_SYNC);
        refresh_parent(shared, &r.path).await;
        return;
    }
    if len == 0 {
        fail(offset, STATUS_CLOUD_FILE_INVALID_REQUEST);
        return;
    }
    let body = match shared.ctx.client.read(&dev, &id, start, len).await {
        Ok((total, _)) if total.is_some_and(|t| t != file_size) => {
            tracing::info!(path = %r.path.display(), "file changed on its device; refreshing");
            fail(start, STATUS_CLOUD_FILE_NOT_IN_SYNC);
            refresh_parent(shared, &r.path).await;
            return;
        }
        Ok((_, body)) => body,
        Err(e) => {
            tracing::info!(path = %r.path.display(), error = %e, "could not download a file of the virtual drive");
            fail(start, status_of(&e));
            if matches!(e, ClientError::NotFound(_)) {
                refresh_parent(shared, &r.path).await;
            }
            return;
        }
    };

    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(CHUNK * 2);
    let mut at = start;
    let end = start + len;
    while at < end {
        // Fill up to a whole piece, or to the end of the range.
        let want = ((end - at) as usize).min(CHUNK);
        while buf.len() < want {
            match stream.next().await {
                Some(Ok(bytes)) => buf.extend_from_slice(&bytes),
                Some(Err(e)) => {
                    tracing::info!(path = %r.path.display(), error = %e, "download interrupted");
                    fail(at, STATUS_CLOUD_FILE_NETWORK_UNAVAILABLE);
                    return;
                }
                None => {
                    tracing::info!(path = %r.path.display(), "download ended early");
                    fail(at, STATUS_CLOUD_FILE_NETWORK_UNAVAILABLE);
                    return;
                }
            }
        }
        if let Err(e) = transfer_data(r, at, &buf[..want], want as u64, STATUS_SUCCESS) {
            // Windows gave up on the request (the app closed the file).
            tracing::debug!(path = %r.path.display(), error = %e, "download no longer wanted");
            return;
        }
        buf.drain(..want);
        at += want as u64;
        unsafe {
            let _ = CfReportProviderProgress(r.key, r.transfer, len as i64, (at - start) as i64);
        }
    }
}

// ---------------------------------------------------------------------------
// Keeping listed folders up to date

/// The placeholders (and files of your own) in a folder.
fn local_children(dir: &Path) -> Vec<Local> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| Local { name: e.file_name().to_string_lossy().into_owned(), identity: ops::identity(&e.path()) })
        .collect()
}

/// Brings one listed folder in line with its device. Returns false when
/// something could not be applied (the next check tries again).
async fn refresh_folder(shared: &Arc<Shared>, dir: PathBuf, identity: Option<Identity>) -> bool {
    let desired = match shared.ctx.client.children(identity.as_ref()).await {
        Ok(d) => d,
        // Gone: the parent folder's refresh removes it.
        Err(ClientError::NotFound(_)) => return true,
        Err(e) => {
            tracing::debug!(path = %dir.display(), error = %e, "not refreshed");
            return false;
        }
    };
    let task_shared = shared.clone();
    tokio::task::spawn_blocking(move || apply_changes(&task_shared, &dir, &desired)).await.unwrap_or(false)
}

fn apply_changes(shared: &Shared, dir: &Path, desired: &[Child]) -> bool {
    if !dir.is_dir() {
        return true;
    }
    let changes = tree::plan(&local_children(dir), desired);
    if changes.is_empty() {
        return true;
    }
    tracing::debug!(path = %dir.display(), changes = changes.len(), "updating virtual drive folder");
    let mut ok = true;
    let mut creates = Vec::new();
    let mut rehydrate = Vec::new();
    for change in changes {
        match change {
            Change::Remove { name } => {
                let path = dir.join(&name);
                let is_dir = path.is_dir();
                match ops::remove(&path, is_dir) {
                    Ok(_) => forget(shared, &path),
                    Err(e) => {
                        tracing::info!(path = %path.display(), error = %e, "could not remove an item deleted on its device");
                        ok = false;
                    }
                }
            }
            Change::Rename { from, to } => {
                let (old, new) = (dir.join(&from), dir.join(&to));
                match std::fs::rename(&old, &new) {
                    Ok(()) => moved(shared, &old, &new),
                    Err(e) => {
                        tracing::info!(from = %old.display(), to = %new.display(), error = %e, "could not rename");
                        ok = false;
                    }
                }
            }
            Change::Update { name, child, content } => {
                let path = dir.join(&name);
                let pinned = std::fs::metadata(&path).map(|m| Attrs::of(&m).pinned()).unwrap_or(false);
                match ops::update(&path, &child, content, pinned) {
                    Ok(()) => {
                        if let Some(entry) = shared.listed.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&path) {
                            *entry = Some(child.identity.clone());
                        }
                        let frequent = shared.cache.settings.lock().unwrap_or_else(|e| e.into_inner()).keep_frequent
                            && shared
                                .usage
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get(&path)
                                .is_some_and(|u| u.frequent(nexus_common::time::now_ms()));
                        if content && (pinned || frequent) {
                            rehydrate.push(path);
                        }
                    }
                    Err(e) => {
                        // Changed here too, or open in an app: try again later.
                        tracing::info!(path = %path.display(), error = %e, "could not update a changed file");
                        ok = false;
                    }
                }
            }
            Change::Create { child } => creates.push(child),
        }
    }
    if !creates.is_empty()
        && let Err(e) = ops::create(dir, &creates)
    {
        tracing::info!(error = %e, "could not add new items");
        ok = false;
    }
    // Kept on this device: download what is new or changed.
    let dir_pinned = std::fs::metadata(dir).map(|m| Attrs::of(&m).pinned()).unwrap_or(false);
    if dir_pinned {
        hydrate_tree(dir);
    } else {
        for path in rehydrate {
            if let Err(e) = ops::hydrate(&path) {
                tracing::info!(path = %path.display(), error = %e, "could not download a changed file kept on this device");
            }
        }
    }
    ok
}

/// Forgets listed folders at or under `path` (removed).
fn forget(shared: &Shared, path: &Path) {
    shared.listed.lock().unwrap_or_else(|e| e.into_inner()).retain(|p, _| !p.starts_with(path));
}

/// Moves listed folders from under `old` to under `new` (renamed).
fn moved(shared: &Shared, old: &Path, new: &Path) {
    let mut listed = shared.listed.lock().unwrap_or_else(|e| e.into_inner());
    let affected: Vec<PathBuf> = listed.keys().filter(|p| p.starts_with(old)).cloned().collect();
    for path in affected {
        if let Some(id) = listed.remove(&path) {
            let rest = path.strip_prefix(old).unwrap_or(Path::new(""));
            listed.insert(new.join(rest), id);
        }
    }
}

/// Refreshes the folder holding `path`, if it was listed.
async fn refresh_parent(shared: &Arc<Shared>, path: &Path) {
    let Some(parent) = path.parent().map(Path::to_path_buf) else { return };
    let identity = shared.listed.lock().unwrap_or_else(|e| e.into_inner()).get(&parent).cloned();
    if let Some(identity) = identity {
        refresh_folder(shared, parent, identity).await;
    }
}

/// Listed folders matching `filter`, parents first.
fn listed_where(shared: &Shared, filter: impl Fn(&Option<Identity>) -> bool) -> Vec<(PathBuf, Option<Identity>)> {
    let mut out: Vec<_> = shared
        .listed
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(_, id)| filter(id))
        .map(|(p, id)| (p.clone(), id.clone()))
        .collect();
    out.sort_by_key(|(p, _)| p.components().count());
    out
}

/// Finds the folders already listed (from earlier runs) without listing
/// any new ones.
fn find_listed(shared: &Shared) {
    fn walk(shared: &Shared, dir: &Path, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_dir() || Attrs::of(&meta).unlisted() || depth > 64 {
                continue;
            }
            let path = entry.path();
            let Some(identity) = ops::identity(&path) else { continue };
            shared.listed.lock().unwrap_or_else(|e| e.into_inner()).insert(path.clone(), Some(identity));
            walk(shared, &path, depth + 1);
        }
    }
    shared.listed.lock().unwrap_or_else(|e| e.into_inner()).insert(shared.root.clone(), None);
    walk(shared, &shared.root, 0);
}

/// Checks a device's storage for changes (at most once a minute) and
/// refreshes the listed folders of locations that changed.
async fn check_device(shared: &Arc<Shared>, dev: &str, force: bool) {
    {
        let mut checked = shared.checked.lock().unwrap_or_else(|e| e.into_inner());
        if !force && checked.get(dev).is_some_and(|t| t.elapsed() < CHECK_EVERY) {
            return;
        }
        checked.insert(dev.to_string(), Instant::now());
    }
    let device_listed = listed_where(shared, |id| matches!(id, Some(Identity::Device { dev: d }) if d == dev));
    if device_listed.is_empty() {
        return;
    }
    let roots = match shared.ctx.client.roots(dev).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "device not checked");
            return;
        }
    };
    for (path, identity) in device_listed {
        refresh_folder(shared, path, identity).await;
    }
    for root in roots {
        let fingerprint = root.fingerprint();
        let previous = shared.fingerprints.lock().unwrap_or_else(|e| e.into_inner()).get(&root.id).cloned();
        if previous.as_deref() == Some(fingerprint.as_str()) {
            continue;
        }
        let folders = listed_where(shared, |id| {
            id.as_ref().is_some_and(|id| id.device() == dev && id.root() == Some(root.id.as_str()))
        });
        let mut ok = true;
        for (path, identity) in folders {
            ok &= refresh_folder(shared, path, identity).await;
        }
        if ok {
            shared.fingerprints.lock().unwrap_or_else(|e| e.into_inner()).insert(root.id.clone(), fingerprint);
        }
    }
}

/// Refreshes the drive's top folder (the list of devices) and shows which
/// devices are offline.
async fn refresh_top(shared: &Arc<Shared>) {
    let root = shared.root.clone();
    refresh_folder(shared, root, None).await;
    for (path, identity) in listed_where(shared, |id| matches!(id, Some(Identity::Section { .. }))) {
        refresh_folder(shared, path, identity).await;
    }
    if let Ok(devices) = shared.ctx.client.all_devices().await {
        for device in devices {
            if device.id != tree::LOCAL {
                show_presence(shared, &device.id, !device.online).await;
            }
            follow(shared, &device.id);
        }
    }
}

/// Follows a device's change feed, so its changes show within seconds.
fn follow(shared: &Arc<Shared>, dev: &str) {
    let mut following = shared.following.lock().unwrap_or_else(|e| e.into_inner());
    if following.contains_key(dev) {
        return;
    }
    let task = shared.ctx.runtime.spawn(follow_changes(shared.clone(), dev.to_string()));
    following.insert(dev.to_string(), task.abort_handle());
}

async fn follow_changes(shared: Arc<Shared>, dev: String) {
    loop {
        let listed = listed_where(&shared, |id| id.as_ref().is_some_and(|i| i.device() == dev && i.root().is_some()));
        if listed.is_empty() {
            // Nothing of this device was opened here yet.
            tokio::time::sleep(Duration::from_secs(20)).await;
            continue;
        }
        let cursor = shared.cursors.lock().unwrap_or_else(|e| e.into_inner()).get(&dev).cloned();
        match shared.ctx.client.changes(&dev, cursor.as_deref(), if cursor.is_some() { FEED_WAIT } else { 0 }).await {
            Ok(changes) => {
                shared.cursors.lock().unwrap_or_else(|e| e.into_inner()).insert(dev.clone(), changes.cursor);
                if cursor.is_none() || changes.reset {
                    // Starting (or starting over): catch up with everything.
                    for (path, identity) in listed {
                        refresh_folder(&shared, path, identity).await;
                    }
                    continue;
                }
                for root in changes.roots {
                    let folders = listed_where(&shared, |id| {
                        id.as_ref().is_some_and(|i| i.device() == dev && i.root() == Some(root.as_str()))
                    });
                    for (path, identity) in folders {
                        refresh_folder(&shared, path, identity).await;
                    }
                }
            }
            Err(ClientError::Offline(_)) => {
                shared.cursors.lock().unwrap_or_else(|e| e.into_inner()).remove(&dev);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Err(e) => {
                // A device without the feed (an older version): the regular
                // checks keep it up to date.
                tracing::debug!(error = %e, "change feed not available");
                tokio::time::sleep(Duration::from_secs(120)).await;
            }
        }
    }
}

/// Marks a device's folder "Device offline" in Explorer (or clears it)
/// when that changed.
async fn show_presence(shared: &Arc<Shared>, dev: &str, offline: bool) {
    if shared.offline.lock().unwrap_or_else(|e| e.into_inner()).get(dev) == Some(&offline) {
        return;
    }
    let root = shared.root.clone();
    let dev_id = dev.to_string();
    let done = tokio::task::spawn_blocking(move || {
        let mine = std::fs::read_dir(&root).ok()?.flatten().map(|e| e.path()).find(|p| {
            matches!(ops::identity(p), Some(Identity::Section { s }) if s == "mine")
        })?;
        let folder = std::fs::read_dir(&mine).ok()?.flatten().map(|e| e.path()).find(|p| {
            matches!(ops::identity(p), Some(Identity::Device { dev }) if dev == dev_id)
        })?;
        // Setting the state counts as a change here: the folder is still in sync.
        let set = register::set_offline(&folder, offline).and_then(|()| ops::mark_in_sync(&folder));
        match set {
            Ok(()) => Some(true),
            Err(e) => {
                tracing::debug!(error = %e, "device state not shown");
                Some(false)
            }
        }
    })
    .await;
    if let Ok(Some(true)) = done {
        shared.offline.lock().unwrap_or_else(|e| e.into_inner()).insert(dev.to_string(), offline);
    }
}

/// Follows the node's events: devices added, renamed or removed change the
/// top folder; a device's presence heartbeat checks its storage.
async fn follow_events(shared: Arc<Shared>) {
    let mut events = shared.ctx.events.subscribe();
    let task_shared = shared.clone();
    if tokio::task::spawn_blocking(move || find_listed(&task_shared)).await.is_err() {
        return;
    }
    refresh_top(&shared).await;
    loop {
        match events.recv().await {
            Ok(Event::DeviceOnline { device_id, .. }) => {
                show_presence(&shared, &device_id, false).await;
                check_device(&shared, &device_id, false).await;
                follow(&shared, &device_id);
            }
            Ok(Event::DeviceOffline { device_id }) => show_presence(&shared, &device_id, true).await,
            Ok(Event::DeviceChanged { device_id }) => {
                refresh_top(&shared).await;
                check_device(&shared, &device_id, true).await;
            }
            Ok(Event::DeviceRemoved { .. }) => refresh_top(&shared).await,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

// ---------------------------------------------------------------------------
// "Always keep on this device" and "Free up space"

type PinEvents = tokio::sync::mpsc::UnboundedReceiver<PathBuf>;

fn watch_pins(root: &Path, tx: tokio::sync::mpsc::UnboundedSender<PathBuf>) -> notify::Result<notify::RecommendedWatcher> {
    use notify::Watcher;
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && matches!(event.kind, notify::EventKind::Modify(_) | notify::EventKind::Create(_))
        {
            for path in event.paths {
                let _ = tx.send(path);
            }
        }
    })?;
    watcher.watch(root, notify::RecursiveMode::Recursive)?;
    Ok(watcher)
}

async fn handle_pins(shared: Arc<Shared>, mut rx: PinEvents) {
    while let Some(first) = rx.recv().await {
        let mut paths = HashSet::from([first]);
        let settle = tokio::time::sleep(PIN_SETTLE);
        tokio::pin!(settle);
        loop {
            tokio::select! {
                _ = &mut settle => break,
                next = rx.recv() => match next {
                    Some(p) => { paths.insert(p); }
                    None => return,
                },
            }
        }
        let root = shared.root.clone();
        let mut paths: Vec<_> = paths.into_iter().filter(|p| p.starts_with(&root) && *p != root).collect();
        // Parents first: a pinned folder covers what is in it, and a new
        // folder is made on its device before what is in it.
        paths.sort_by_key(|p| p.components().count());
        let pins = paths.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let mut done: Vec<PathBuf> = Vec::new();
            for path in pins {
                if done.iter().any(|d| path.starts_with(d)) {
                    continue;
                }
                if apply_pin(&path) {
                    done.push(path);
                }
            }
        })
        .await;
        for path in paths {
            send_back(&shared, path).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Changes made here

/// Files programs write while saving, which are not worth sending.
fn is_temporary(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("~$")
        || n.starts_with(".~")
        || n.starts_with("~wr")
        || n.ends_with(".tmp")
        || n.ends_with(".crdownload")
        || n.ends_with(".partial")
        || n.ends_with(".part")
        || n == "desktop.ini"
        || n == "thumbs.db"
}

/// What changed here, if anything is to be sent to a device.
enum LocalChange {
    /// A file made here, in a folder of a device.
    NewFile { parent: Identity },
    NewFolder { parent: Identity },
    /// A downloaded file changed here.
    Edited { identity: Identity },
}

fn local_change(root: &Path, path: &Path) -> Option<LocalChange> {
    if !path.starts_with(root) || path == root {
        return None;
    }
    if is_temporary(&path.file_name()?.to_string_lossy()) {
        return None;
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    match ops::identity(path) {
        Some(identity) => {
            let attrs = Attrs::of(&meta);
            if meta.is_dir() || attrs.online_only() || ops::in_sync(path) {
                return None;
            }
            matches!(identity, Identity::Entry { dir: false, .. }).then_some(LocalChange::Edited { identity })
        }
        None => {
            let parent = ops::identity(path.parent()?)?;
            if !matches!(parent, Identity::Root { .. } | Identity::Entry { dir: true, .. }) {
                // Devices and sections only hold what HearthDrive puts there.
                return None;
            }
            Some(if meta.is_dir() { LocalChange::NewFolder { parent } } else { LocalChange::NewFile { parent } })
        }
    }
}

/// The device, storage location and folder something in `parent` goes to.
fn place_of(parent: &Identity) -> Option<(String, String, String)> {
    match parent {
        Identity::Root { dev, root } => Some((dev.clone(), root.clone(), String::new())),
        Identity::Entry { dev, root, path, dir: true, .. } => Some((dev.clone(), root.clone(), path.clone())),
        _ => None,
    }
}

fn identity_of(dev: &str, root: &str, e: &EntryInfo) -> Identity {
    Identity::Entry {
        dev: dev.to_string(),
        root: root.to_string(),
        path: e.relative_path.clone(),
        id: e.id.clone(),
        dir: e.is_dir,
        size: if e.is_dir { 0 } else { e.size.max(0) as u64 },
        mtime: e.modified_at.unwrap_or_default(),
    }
}

/// Size and last change of a file, to notice it changing during an upload.
fn stamp(path: &Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Whether no program is writing the file (it can be opened without
/// letting others write).
fn closed(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().read(true).share_mode(1 /* FILE_SHARE_READ */).open(path).is_ok()
}

fn file_body(file: tokio::fs::File) -> axum::body::Body {
    let stream = futures_util::stream::unfold(file, |mut file| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 1024 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(buf), file))
            }
            Err(e) => Some((Err(e), file)),
        }
    });
    axum::body::Body::from_stream(stream)
}

/// Sends a change made here to its device; a busy file or an unreachable
/// device is tried again later.
async fn send_back(shared: &Arc<Shared>, path: PathBuf) {
    let root = shared.root.clone();
    let look = path.clone();
    let Ok(Some(change)) = tokio::task::spawn_blocking(move || local_change(&root, &look)).await else { return };
    match send_change(shared, &path, change).await {
        Ok(()) => {}
        Err(retry) => {
            if retry {
                let tx = shared.local.clone();
                shared.ctx.runtime.spawn(async move {
                    tokio::time::sleep(RETRY_UPLOAD).await;
                    let _ = tx.send(path);
                });
            }
        }
    }
}

/// `Err(true)` to try again later.
fn send_change<'a>(
    shared: &'a Arc<Shared>,
    path: &'a Path,
    change: LocalChange,
) -> futures_util::future::BoxFuture<'a, Result<(), bool>> {
    Box::pin(async move {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let client = &shared.ctx.client;
        let failed = |e: ClientError| {
            tracing::info!(path = %path.display(), error = %e, "could not send a change to its device");
            Err(matches!(e, ClientError::Offline(_)))
        };
        match change {
            LocalChange::NewFolder { parent } => {
                let Some((dev, root, parent_path)) = place_of(&parent) else { return Ok(()) };
                let entry = match client.create_folder(&dev, &root, &parent_path, &name).await {
                    Ok(e) => e,
                    Err(e) => return failed(e),
                };
                let identity = identity_of(&dev, &root, &entry);
                let (p, id) = (path.to_path_buf(), identity.clone());
                let converted = tokio::task::spawn_blocking(move || ops::convert(&p, &id)).await.unwrap_or(Err("stopped".into()));
                if let Err(e) = converted {
                    tracing::info!(path = %path.display(), error = %e, "folder made on its device, but not here");
                    return Ok(());
                }
                shared.listed.lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_path_buf(), Some(identity));
                tracing::info!(path = %path.display(), "folder made on its device");
                // What was put in it.
                let children: Vec<PathBuf> = std::fs::read_dir(path).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
                for child in children {
                    let root = shared.root.clone();
                    let look = child.clone();
                    if let Ok(Some(change)) = tokio::task::spawn_blocking(move || local_change(&root, &look)).await
                        && let Err(true) = send_change(shared, &child, change).await
                    {
                        let _ = shared.local.send(child);
                    }
                }
                Ok(())
            }
            LocalChange::NewFile { parent } => {
                let Some((dev, root, parent_path)) = place_of(&parent) else { return Ok(()) };
                if !closed(path) {
                    return Err(true);
                }
                let Some(before) = stamp(path) else { return Ok(()) };
                let Ok(file) = tokio::fs::File::open(path).await else { return Err(true) };
                let entry = match client.upload(&dev, &root, &parent_path, &name, before.0, file_body(file), false).await {
                    Ok(e) => e,
                    Err(e) => return failed(e),
                };
                let identity = identity_of(&dev, &root, &entry);
                let unchanged = stamp(path) == Some(before);
                let p = path.to_path_buf();
                let done = tokio::task::spawn_blocking(move || {
                    ops::convert(&p, &identity)?;
                    if !unchanged {
                        // Changed while it was sent: send it again.
                        ops::set_identity(&p, &identity)?;
                    }
                    Ok::<_, String>(())
                })
                .await
                .unwrap_or(Err("stopped".into()));
                match done {
                    Ok(()) => tracing::info!(path = %path.display(), "new file sent to its device"),
                    Err(e) => tracing::info!(path = %path.display(), error = %e, "file sent, but not marked here"),
                }
                if unchanged { Ok(()) } else { Err(true) }
            }
            LocalChange::Edited { identity } => {
                let Identity::Entry { dev, root, path: rel, .. } = &identity else { return Ok(()) };
                if !closed(path) {
                    return Err(true);
                }
                let Some(before) = stamp(path) else { return Ok(()) };
                let (parent_path, file_name) = match rel.rsplit_once('/') {
                    Some((p, n)) => (p.to_string(), n.to_string()),
                    None => (String::new(), rel.clone()),
                };
                let Ok(file) = tokio::fs::File::open(path).await else { return Err(true) };
                let entry = match client.upload(dev, root, &parent_path, &file_name, before.0, file_body(file), true).await {
                    Ok(e) => e,
                    Err(e) => return failed(e),
                };
                let updated = identity_of(dev, root, &entry);
                let unchanged = stamp(path) == Some(before);
                let p = path.to_path_buf();
                let _ = tokio::task::spawn_blocking(move || {
                    if unchanged { ops::set_synced(&p, &updated) } else { ops::set_identity(&p, &updated) }
                })
                .await;
                tracing::info!(path = %path.display(), "changes sent to its device");
                if unchanged { Ok(()) } else { Err(true) }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// The offline cache

fn usage_file(data_dir: &Path) -> PathBuf {
    data_dir.join("vdrive-usage.json")
}

fn load_usage(data_dir: &Path) -> HashMap<PathBuf, Usage> {
    std::fs::read(usage_file(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<(PathBuf, Usage)>>(&b).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

fn save_usage(shared: &Shared) {
    let mut list: Vec<(PathBuf, Usage)> =
        shared.usage.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(p, u)| (p.clone(), *u)).collect();
    list.sort_by_key(|(_, u)| std::cmp::Reverse(u.last));
    list.truncate(50_000);
    if let Ok(bytes) = serde_json::to_vec(&list) {
        let _ = std::fs::write(usage_file(&shared.ctx.data_dir), bytes);
    }
}

async fn keep_cache(shared: Arc<Shared>) {
    // Let the drive settle first.
    tokio::time::sleep(Duration::from_secs(60)).await;
    loop {
        let task_shared = shared.clone();
        let _ = tokio::task::spawn_blocking(move || {
            sweep(&task_shared);
            save_usage(&task_shared);
        })
        .await;
        tokio::select! {
            _ = tokio::time::sleep(CACHE_EVERY) => {}
            _ = shared.cache.wake.notified() => {}
        }
    }
}

/// Clears downloaded files not used for a while, then the least recently
/// used ones while the cache is over its limit. Files kept on purpose,
/// often used files and files changed here are never cleared.
fn sweep(shared: &Shared) {
    let settings = shared.cache.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let now = nexus_common::time::now_ms();
    let usage = shared.usage.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let folders: Vec<PathBuf> = shared.listed.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
    // (path, size, last use, often used)
    let mut files: Vec<(PathBuf, u64, i64, bool)> = Vec::new();
    let mut status = crate::CacheStatus::default();
    for dir in folders {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                continue;
            }
            let attrs = Attrs::of(&meta);
            if attrs.online_only() {
                continue;
            }
            let path = entry.path();
            if ops::identity(&path).is_none() {
                continue;
            }
            if attrs.pinned() {
                status.kept_bytes += meta.len();
                continue;
            }
            if !ops::in_sync(&path) {
                continue;
            }
            let used = usage.get(&path);
            let last = used.map(|u| u.last).unwrap_or_else(|| {
                meta.accessed()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(now)
            });
            let frequent = settings.keep_frequent && used.is_some_and(|u| u.frequent(now));
            if frequent {
                status.frequent += 1;
            }
            files.push((path, meta.len(), last, frequent));
        }
    }
    let mut cleared = 0u64;
    let mut clear = |path: &Path| match ops::dehydrate(path) {
        Ok(()) => {
            cleared += 1;
            true
        }
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "not cleared");
            false
        }
    };
    if settings.clear_after_days > 0 {
        let cutoff = now - settings.clear_after_days as i64 * DAY_MS;
        files.retain(|(path, _, last, frequent)| !(!*frequent && *last < cutoff && clear(path)));
    }
    let mut used: u64 = files.iter().map(|f| f.1).sum();
    let limit = settings.limit_bytes();
    if limit > 0 && used > limit {
        // Often used files go last; otherwise the least recently used first.
        files.sort_by_key(|(_, _, last, frequent)| (*frequent, *last));
        let target = limit / 10 * 9;
        let mut kept = Vec::new();
        for f in files {
            if used > target && clear(&f.0) {
                used -= f.1;
            } else {
                kept.push(f);
            }
        }
        files = kept;
    }
    if cleared > 0 {
        tracing::info!(files = cleared, "offline cache cleared files");
    }
    shared.cleared.fetch_add(cleared, Ordering::Relaxed);
    status.used_bytes = files.iter().map(|f| f.1).sum();
    status.files = files.len() as u64;
    status.cleared = shared.cleared.load(Ordering::Relaxed);
    status.checked_at = Some(now);
    *shared.cache.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
    // Forget files that are gone.
    shared.usage.lock().unwrap_or_else(|e| e.into_inner()).retain(|p, _| p.exists());
}

/// Acts on a pin state change. Returns true when a whole folder was handled.
fn apply_pin(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    let attrs = Attrs::of(&meta);
    if meta.is_dir() {
        if attrs.pinned() {
            hydrate_tree(path);
            return true;
        }
        if attrs.unpinned() {
            dehydrate_tree(path);
            return true;
        }
        return false;
    }
    if attrs.pinned() && attrs.online_only() {
        if let Err(e) = ops::hydrate(path) {
            tracing::info!(path = %path.display(), error = %e, "could not keep a file on this device");
        }
    } else if attrs.unpinned()
        && !attrs.online_only()
        && ops::in_sync(path)
        && let Err(e) = ops::dehydrate(path)
    {
        tracing::info!(path = %path.display(), error = %e, "could not free up space");
    }
    false
}

/// Downloads everything in a folder kept on this device (listing folders
/// as needed), except what was set to free up space.
fn hydrate_tree(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let attrs = Attrs::of(&meta);
        if attrs.unpinned() {
            continue;
        }
        let path = entry.path();
        if meta.is_dir() {
            hydrate_tree(&path);
        } else if attrs.online_only()
            && let Err(e) = ops::hydrate(&path)
        {
            tracing::info!(path = %path.display(), error = %e, "could not keep a file on this device");
        }
    }
}

/// Frees the space of everything downloaded in a folder, except what is
/// kept on this device or was changed here. Folders never listed hold
/// nothing and are skipped.
fn dehydrate_tree(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let attrs = Attrs::of(&meta);
        if attrs.pinned() {
            continue;
        }
        let path = entry.path();
        if meta.is_dir() {
            if !attrs.unlisted() {
                dehydrate_tree(&path);
            }
        } else if !attrs.online_only()
            && ops::in_sync(&path)
            && let Err(e) = ops::dehydrate(&path)
        {
            tracing::info!(path = %path.display(), error = %e, "could not free up space");
        }
    }
}
