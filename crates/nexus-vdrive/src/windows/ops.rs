//! Placeholder operations: creating, reading, updating, downloading and
//! freeing placeholders on disk.

use crate::tree::{Child, Identity, file_time};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::CloudFilters::*;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_PINNED,
    FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_UNPINNED, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::core::PCWSTR;

const FILE_READ_DATA: u32 = 0x1;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
const WRITE_DAC: u32 = 0x40000;

pub fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain([0]).collect()
}

/// An open file or folder, closed when dropped.
pub struct Handle(pub HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Opens a placeholder without reading (and so without downloading) it.
pub fn open(path: &Path, access: u32) -> windows::core::Result<Handle> {
    let name = wide(path);
    unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
        .map(Handle)
    }
}

/// Attribute bits that tell a placeholder's state.
#[derive(Debug, Clone, Copy)]
pub struct Attrs(pub u32);

impl Attrs {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        Attrs(meta.file_attributes())
    }
    pub fn pinned(self) -> bool {
        self.0 & FILE_ATTRIBUTE_PINNED.0 != 0
    }
    pub fn unpinned(self) -> bool {
        self.0 & FILE_ATTRIBUTE_UNPINNED.0 != 0
    }
    /// A file not (completely) on this computer.
    pub fn online_only(self) -> bool {
        self.0 & FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0 != 0
    }
    /// A folder whose contents have not been listed yet: Windows marks it
    /// like a file not on this computer.
    pub fn unlisted(self) -> bool {
        self.0 & (FILE_ATTRIBUTE_RECALL_ON_OPEN.0 | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0) != 0
    }
}

/// Placeholder information followed by room for its identity.
#[repr(C, align(8))]
struct InfoBuffer([u8; 512 + crate::tree::MAX_IDENTITY]);

/// The identity stored with a placeholder; `None` for anything that is not
/// one of this drive's placeholders.
pub fn identity(path: &Path) -> Option<Identity> {
    let handle = open(path, FILE_READ_ATTRIBUTES).ok()?;
    let mut buf = Box::new(InfoBuffer([0; 512 + crate::tree::MAX_IDENTITY]));
    unsafe {
        CfGetPlaceholderInfo(
            handle.0,
            CF_PLACEHOLDER_INFO_BASIC,
            buf.0.as_mut_ptr().cast(),
            buf.0.len() as u32,
            None,
        )
        .ok()?;
        let info = &*(buf.0.as_ptr() as *const CF_PLACEHOLDER_BASIC_INFO);
        let start = std::mem::offset_of!(CF_PLACEHOLDER_BASIC_INFO, FileIdentity);
        let len = (info.FileIdentityLength as usize).min(buf.0.len() - start);
        Identity::decode(&buf.0[start..start + len])
    }
}

/// Whether a placeholder is in sync with its device (not changed here).
pub fn in_sync(path: &Path) -> bool {
    let Ok(handle) = open(path, FILE_READ_ATTRIBUTES) else { return false };
    let mut buf = Box::new(InfoBuffer([0; 512 + crate::tree::MAX_IDENTITY]));
    unsafe {
        CfGetPlaceholderInfo(handle.0, CF_PLACEHOLDER_INFO_BASIC, buf.0.as_mut_ptr().cast(), buf.0.len() as u32, None)
            .is_ok()
            && (*(buf.0.as_ptr() as *const CF_PLACEHOLDER_BASIC_INFO)).InSyncState == CF_IN_SYNC_STATE_IN_SYNC
    }
}

/// Size, times and attributes Windows shows for an item.
pub fn metadata(child: &Child) -> CF_FS_METADATA {
    let now = file_time(nexus_common::time::now_ms());
    let modified = child.modified().map(file_time).unwrap_or(now);
    let created = child.created.filter(|c| *c > 0).map(file_time).unwrap_or(modified);
    let attributes = if child.identity.is_dir() { FILE_ATTRIBUTE_DIRECTORY.0 } else { FILE_ATTRIBUTE_NORMAL.0 };
    CF_FS_METADATA {
        BasicInfo: FILE_BASIC_INFO {
            CreationTime: created,
            LastAccessTime: modified,
            LastWriteTime: modified,
            ChangeTime: modified,
            FileAttributes: attributes,
        },
        FileSize: child.size() as i64,
    }
}

/// Placeholder descriptions for `children`, valid while `storage` lives.
pub struct CreateInfos {
    pub infos: Vec<CF_PLACEHOLDER_CREATE_INFO>,
    _names: Vec<Vec<u16>>,
    _identities: Vec<Vec<u8>>,
}

pub fn create_infos(children: &[Child]) -> CreateInfos {
    let mut names = Vec::new();
    let mut identities = Vec::new();
    let mut infos = Vec::new();
    for child in children {
        let Some(identity) = child.identity.encode() else {
            tracing::warn!(name = %child.name, "path too long for the virtual drive; skipped");
            continue;
        };
        let name: Vec<u16> = child.name.encode_utf16().chain([0]).collect();
        infos.push(CF_PLACEHOLDER_CREATE_INFO {
            RelativeFileName: PCWSTR(name.as_ptr()),
            FsMetadata: metadata(child),
            FileIdentity: identity.as_ptr().cast(),
            FileIdentityLength: identity.len() as u32,
            Flags: CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC,
            Result: Default::default(),
            CreateUsn: 0,
        });
        // The vectors' heap buffers do not move when the vectors do.
        names.push(name);
        identities.push(identity);
    }
    CreateInfos { infos, _names: names, _identities: identities }
}

/// Creates placeholders in `dir` (a folder already listed).
pub fn create(dir: &Path, children: &[Child]) -> Result<(), String> {
    let mut set = create_infos(children);
    if set.infos.is_empty() {
        return Ok(());
    }
    let base = wide(dir);
    let mut done = 0u32;
    let result = unsafe { CfCreatePlaceholders(PCWSTR(base.as_ptr()), &mut set.infos, CF_CREATE_FLAG_NONE, Some(&mut done)) };
    for (info, child) in set.infos.iter().zip(children) {
        if info.Result.is_err() {
            tracing::debug!(name = %child.name, error = %windows::core::Error::from(info.Result), "placeholder not created");
        }
    }
    result.map_err(|e| format!("creating placeholders in {}: {}", dir.display(), e.message()))
}

/// Gives a placeholder new metadata and identity. With `drop_content`, a
/// downloaded copy is dropped (the file changed on its device); this fails
/// rather than lose changes made to the file here. A file kept on this
/// device (`pinned`) stays kept; the caller downloads it again.
pub fn update(path: &Path, child: &Child, drop_content: bool, pinned: bool) -> Result<(), String> {
    let handle =
        open(path, WRITE_DAC | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES).map_err(|e| e.message().to_string())?;
    // Windows never drops the content of a pinned file: unpin it meanwhile.
    let unpin = drop_content && pinned && !child.identity.is_dir();
    if unpin {
        unsafe { CfSetPinState(handle.0, CF_PIN_STATE_UNSPECIFIED, CF_SET_PIN_FLAG_NONE, None) }
            .map_err(|e| e.message().to_string())?;
    }
    let result = update_handle(&handle, child, drop_content);
    if unpin {
        unsafe {
            let _ = CfSetPinState(handle.0, CF_PIN_STATE_PINNED, CF_SET_PIN_FLAG_NONE, None);
        }
    }
    result
}

fn update_handle(handle: &Handle, child: &Child, drop_content: bool) -> Result<(), String> {
    let meta = metadata(child);
    let identity = child.identity.encode().ok_or("path too long")?;
    let mut flags = CF_UPDATE_FLAG_MARK_IN_SYNC | CF_UPDATE_FLAG_VERIFY_IN_SYNC;
    if drop_content && !child.identity.is_dir() {
        flags |= CF_UPDATE_FLAG_DEHYDRATE;
    }
    unsafe {
        CfUpdatePlaceholder(
            handle.0,
            Some(&meta),
            Some(identity.as_ptr().cast()),
            identity.len() as u32,
            None,
            flags,
            None,
            None,
        )
    }
    .map_err(|e| e.message().to_string())
}

/// Marks a placeholder in sync again (after NEXUS itself changed it).
pub fn mark_in_sync(path: &Path) -> Result<(), String> {
    let handle = open(path, WRITE_DAC | FILE_READ_ATTRIBUTES).map_err(|e| e.message().to_string())?;
    unsafe { CfSetInSyncState(handle.0, CF_IN_SYNC_STATE_IN_SYNC, CF_SET_IN_SYNC_FLAG_NONE, None) }
        .map_err(|e| e.message().to_string())
}

/// Downloads a whole file now (it stays on this computer).
pub fn hydrate(path: &Path) -> Result<(), String> {
    let handle = open(path, FILE_READ_DATA | FILE_READ_ATTRIBUTES).map_err(|e| e.message().to_string())?;
    unsafe { CfHydratePlaceholder(handle.0, 0, -1, CF_HYDRATE_FLAG_NONE, None) }.map_err(|e| e.message().to_string())
}

/// Frees the space a downloaded file takes; it stays listed.
pub fn dehydrate(path: &Path) -> Result<(), String> {
    let handle = open(path, WRITE_DAC | FILE_READ_ATTRIBUTES).map_err(|e| e.message().to_string())?;
    unsafe { CfDehydratePlaceholder(handle.0, 0, -1, CF_DEHYDRATE_FLAG_NONE, None) }.map_err(|e| e.message().to_string())
}

/// Removes a placeholder, and a folder placeholder with everything of the
/// drive's in it. Files of your own (not placeholders) and files changed
/// here are kept, with the folders holding them. Folders never listed are
/// not listed now.
pub fn remove(path: &Path, is_dir: bool) -> std::io::Result<bool> {
    if identity(path).is_none() {
        return Ok(false);
    }
    if !is_dir {
        if !in_sync(path) {
            tracing::info!(path = %path.display(), "kept a file changed on this computer");
            return Ok(false);
        }
        std::fs::remove_file(path)?;
        return Ok(true);
    }
    if !remove_contents(path)? {
        return Ok(false);
    }
    std::fs::remove_dir(path)?;
    Ok(true)
}

/// Removes the drive's placeholders inside `dir`. True when nothing is left.
pub fn remove_contents(dir: &Path) -> std::io::Result<bool> {
    let mut kept = false;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        let attrs = Attrs::of(&meta);
        let child = entry.path();
        let removed = if meta.is_dir() && attrs.unlisted() && identity(&child).is_some() {
            std::fs::remove_dir(&child).is_ok()
        } else {
            remove(&child, meta.is_dir()).unwrap_or(false)
        };
        kept |= !removed;
    }
    Ok(!kept)
}
