//! Registering the drive's folder with Windows as a cloud sync root, which
//! also puts it in File Explorer's navigation pane with the HearthDrive icon.

use std::path::{Path, PathBuf};
use windows::Security::Cryptography::{BinaryStringEncoding, CryptographicBuffer};
use windows::Storage::IStorageItem;
use windows::Storage::Provider::{
    StorageProviderItemProperties, StorageProviderItemProperty, StorageProviderItemPropertyDefinition,
    StorageProviderHardlinkPolicy, StorageProviderHydrationPolicy, StorageProviderHydrationPolicyModifier,
    StorageProviderInSyncPolicy, StorageProviderPopulationPolicy, StorageProviderProtectionMode,
    StorageProviderSyncRootInfo, StorageProviderSyncRootManager,
};
use windows::Storage::StorageFolder;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, Interface, PWSTR};

/// The custom state marking a device that cannot be reached.
const OFFLINE_STATE: i32 = 1;

/// Names this provider in sync root ids.
const PROVIDER: &str = "NEXUSCloud";

static ICON: &[u8] = include_bytes!("../../../../apps/desktop/src-tauri/icons/icon.ico");

fn err(what: &str, e: windows::core::Error) -> String {
    format!("{what}: {}", e.message())
}

/// The signed-in Windows account's SID (`S-1-5-21-…`).
fn user_sid() -> Result<String, String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(|e| err("Reading your Windows account", e))?;
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        let read = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), needed, &mut needed);
        let _ = CloseHandle(token);
        read.map_err(|e| err("Reading your Windows account", e))?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).map_err(|e| err("Reading your Windows account", e))?;
        let sid = text.to_string().map_err(|e| e.to_string());
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

/// `NEXUSCloud!<SID>!<device id>`: one drive per node and Windows account.
pub fn sync_root_id(account: &str) -> Result<String, String> {
    let account: String = account.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    Ok(format!("{PROVIDER}!{}!{account}", user_sid()?))
}

/// The folder registered under `id`, if any.
pub fn registered_path(id: &str) -> Option<PathBuf> {
    let roots = StorageProviderSyncRootManager::GetCurrentSyncRoots().ok()?;
    for info in roots {
        if info.Id().ok()?.to_string_lossy() == id {
            return info.Path().ok()?.Path().ok().map(|p| PathBuf::from(p.to_os_string()));
        }
    }
    None
}

/// The icon file Explorer shows for the drive, kept in the data folder.
fn icon_path(data_dir: &Path) -> Result<PathBuf, String> {
    // Explorer reads the path as text: Windows separators only.
    let dir = PathBuf::from(data_dir.to_string_lossy().replace('/', "\\")).join("vdrive");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Creating {}: {e}", dir.display()))?;
    let path = dir.join("nexus.ico");
    if std::fs::read(&path).ok().as_deref() != Some(ICON) {
        std::fs::write(&path, ICON).map_err(|e| format!("Writing {}: {e}", path.display()))?;
    }
    Ok(path)
}

/// Registers `path` as the sync root `id` (updating an earlier
/// registration of the same folder).
pub fn register(id: &str, path: &Path, data_dir: &Path) -> Result<(), String> {
    let icon = icon_path(data_dir)?;
    let build = || -> windows::core::Result<()> {
        let info = StorageProviderSyncRootInfo::new()?;
        info.SetId(&HSTRING::from(id))?;
        let folder = StorageFolder::GetFolderFromPathAsync(&HSTRING::from(path.as_os_str()))?.join()?;
        info.SetPath(&folder)?;
        info.SetDisplayNameResource(&HSTRING::from("HearthDrive"))?;
        info.SetIconResource(&HSTRING::from(format!("{},0", icon.display())))?;
        // Files are downloaded piece by piece as they are read, so a video
        // can start before all of it arrived.
        info.SetHydrationPolicy(StorageProviderHydrationPolicy::Progressive)?;
        info.SetHydrationPolicyModifier(StorageProviderHydrationPolicyModifier::AutoDehydrationAllowed)?;
        // Folders are listed by HearthDrive when first opened.
        info.SetPopulationPolicy(StorageProviderPopulationPolicy::Full)?;
        info.SetInSyncPolicy(StorageProviderInSyncPolicy::Default)?;
        info.SetHardlinkPolicy(StorageProviderHardlinkPolicy::None)?;
        info.SetProtectionMode(StorageProviderProtectionMode::Unknown)?;
        info.SetVersion(&HSTRING::from(nexus_common::VERSION))?;
        info.SetShowSiblingsAsGroup(false)?;
        info.SetAllowPinning(true)?;
        let offline = StorageProviderItemPropertyDefinition::new()?;
        offline.SetId(OFFLINE_STATE)?;
        offline.SetDisplayNameResource(&HSTRING::from("Device status"))?;
        info.StorageProviderItemPropertyDefinitions()?.Append(&offline)?;
        info.SetContext(&CryptographicBuffer::ConvertStringToBinary(&HSTRING::from(id), BinaryStringEncoding::Utf8)?)?;
        StorageProviderSyncRootManager::Register(&info)
    };
    build().map_err(|e| err("Windows did not accept the virtual drive", e))
}

/// Shows (or clears) "Device offline" on a device's folder in Explorer.
pub fn set_offline(folder: &Path, offline: bool) -> Result<(), String> {
    let apply = || -> windows::core::Result<()> {
        let item: IStorageItem =
            StorageFolder::GetFolderFromPathAsync(&HSTRING::from(folder.as_os_str()))?.join()?.cast()?;
        let mut properties = Vec::new();
        if offline {
            let state = StorageProviderItemProperty::new()?;
            state.SetId(OFFLINE_STATE)?;
            state.SetValue(&HSTRING::from("Device offline"))?;
            // The "disconnected" overlay from imageres.dll.
            state.SetIconResource(&HSTRING::from("imageres.dll,-1043"))?;
            properties.push(Some(state));
        }
        let list: windows_collections::IIterable<StorageProviderItemProperty> = properties.into();
        StorageProviderItemProperties::SetAsync(&item, &list)?.join()
    };
    apply().map_err(|e| err("Marking a device offline", e))
}

pub fn unregister(id: &str) -> Result<(), String> {
    StorageProviderSyncRootManager::Unregister(&HSTRING::from(id)).map_err(|e| err("Removing the virtual drive", e))
}
