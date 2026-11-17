//! Drive detection and free-space queries.

use serde::Serialize;
use std::path::Path;
use sysinfo::{DiskKind, Disks};

#[derive(Debug, Clone, Serialize)]
pub struct DriveInfo {
    pub mount_point: String,
    pub label: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub removable: bool,
    pub kind: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

pub fn list_drives() -> Vec<DriveInfo> {
    let disks = Disks::new_with_refreshed_list();
    let mut drives: Vec<DriveInfo> = disks
        .list()
        .iter()
        .map(|d| DriveInfo {
            mount_point: d.mount_point().to_string_lossy().into_owned(),
            label: d.name().to_string_lossy().into_owned(),
            file_system: d.file_system().to_string_lossy().into_owned(),
            total_bytes: d.total_space(),
            available_bytes: d.available_space(),
            removable: d.is_removable(),
            kind: match d.kind() {
                DiskKind::HDD => "hdd",
                DiskKind::SSD => "ssd",
                _ => "unknown",
            },
        })
        .collect();
    drives.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    drives.dedup_by(|a, b| a.mount_point == b.mount_point);
    drives
}

/// The drive containing `path` (longest matching mount point).
pub fn drive_for(path: &Path) -> Option<DriveInfo> {
    let target = path.to_string_lossy().to_lowercase();
    list_drives()
        .into_iter()
        .filter(|d| target.starts_with(&d.mount_point.to_lowercase()))
        .max_by_key(|d| d.mount_point.len())
}

pub fn space_for(path: &Path) -> Option<DiskSpace> {
    drive_for(path).map(|d| DiskSpace { total_bytes: d.total_bytes, available_bytes: d.available_bytes })
}
