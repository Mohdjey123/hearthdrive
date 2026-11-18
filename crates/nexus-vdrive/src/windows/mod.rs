//! The drive on Windows: a Cloud Files API sync provider.
//!
//! Windows calls back into this process when Explorer or any app opens a
//! folder that was never listed (`FETCH_PLACEHOLDERS`) or reads a file that
//! is not on this computer yet (`FETCH_DATA`). Callbacks only copy what they
//! need and hand the work to the node's async runtime, which answers Windows
//! with `CfExecute` when the device has replied.

mod ops;
mod register;

use crate::Context;
use crate::client::ClientError;
use crate::tree::{self, Change, Child, Identity, Local, transfer_range};
use futures_util::StreamExt;
use nexus_common::events::Event;
use ops::Attrs;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use windows::Storage::Provider::StorageProviderSyncRootManager;
use windows::Win32::Foundation::{
    NTSTATUS, STATUS_CLOUD_FILE_INVALID_REQUEST, STATUS_CLOUD_FILE_NETWORK_UNAVAILABLE, STATUS_CLOUD_FILE_NOT_IN_SYNC,
    STATUS_CLOUD_FILE_READ_ONLY_VOLUME, STATUS_CLOUD_FILE_UNSUCCESSFUL, STATUS_SUCCESS,
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
    pub fn start(ctx: Arc<Context>, path: &Path) -> Result<Self, String> {
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

        let shared = Arc::new(Shared {
            ctx: ctx.clone(),
            root: root.clone(),
            listed: Mutex::new(HashMap::new()),
            transfers: Mutex::new(HashMap::new()),
            next_transfer: Default::default(),
            fingerprints: Mutex::new(HashMap::new()),
            checked: Mutex::new(HashMap::new()),
            offline: Mutex::new(HashMap::new()),
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

        let (watcher, pins) = match watch_pins(&root) {
            Ok((w, rx)) => (Some(w), Some(rx)),
            Err(e) => {
                tracing::warn!(error = %e, "cannot watch the virtual drive; Always keep / Free up space will not act");
                (None, None)
            }
        };
        let mut tasks = Vec::new();
        let rt = &ctx.runtime;
        tasks.push(rt.spawn(follow_events(shared.clone())));
        if let Some(rx) = pins {
            tasks.push(rt.spawn(handle_pins(shared.clone(), rx)));
        }
        Ok(Self { shared, id, key, _watcher: watcher, tasks })
    }

    /// Stops serving the drive and removes it from Explorer. With `remove`,
    /// its placeholders and downloaded copies are deleted too (files of
    /// your own stay).
    pub fn stop(self, remove: bool) {
        for task in &self.tasks {
            task.abort();
        }
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

/// Phase 1 is read-only: only this process (bringing the drive up to date)
/// may delete or rename placeholders.
unsafe extern "system" fn on_delete(info: *const CF_CALLBACK_INFO, _params: *const CF_CALLBACK_PARAMETERS) {
    let r = unsafe { request_of(info) };
    let allowed = r.pid == std::process::id();
    if !allowed {
        tracing::info!(path = %r.path.display(), "refused deleting in the read-only virtual drive");
    }
    ack(&r, CF_OPERATION_TYPE_ACK_DELETE, if allowed { STATUS_SUCCESS } else { STATUS_CLOUD_FILE_READ_ONLY_VOLUME });
}

unsafe extern "system" fn on_rename(info: *const CF_CALLBACK_INFO, _params: *const CF_CALLBACK_PARAMETERS) {
    let r = unsafe { request_of(info) };
    let allowed = r.pid == std::process::id();
    if !allowed {
        tracing::info!(path = %r.path.display(), "refused renaming in the read-only virtual drive");
    }
    ack(&r, CF_OPERATION_TYPE_ACK_RENAME, if allowed { STATUS_SUCCESS } else { STATUS_CLOUD_FILE_READ_ONLY_VOLUME });
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
                        if content && pinned {
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
    if let Ok(devices) = shared.ctx.client.devices().await {
        for device in devices {
            show_presence(shared, &device.id, !device.online).await;
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
        let folder = std::fs::read_dir(&root).ok()?.flatten().map(|e| e.path()).find(|p| {
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

fn watch_pins(root: &Path) -> notify::Result<(notify::RecommendedWatcher, PinEvents)> {
    use notify::Watcher;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
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
    Ok((watcher, rx))
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
        let _ = tokio::task::spawn_blocking(move || {
            // Parents first: a pinned folder covers what is in it.
            let mut paths: Vec<_> = paths.into_iter().filter(|p| p.starts_with(&root) && *p != root).collect();
            paths.sort_by_key(|p| p.components().count());
            let mut done: Vec<PathBuf> = Vec::new();
            for path in paths {
                if done.iter().any(|d| path.starts_with(d)) {
                    continue;
                }
                if apply_pin(&path) {
                    done.push(path);
                }
            }
        })
        .await;
    }
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
