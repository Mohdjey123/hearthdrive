use crate::{drives, paths};
use nexus_common::types::{DeletedFile, FileEntry, RootType, StorageRoot};
use nexus_common::{NexusError, Result, new_id, time::now_ms};
use nexus_database::Database;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const STAGING_DIR: &str = "staging";
const TRASH_DIR: &str = "trash";
/// Uploads must leave at least this much free space on the target drive.
const MIN_FREE_MARGIN: u64 = 64 * 1024 * 1024;
const STALE_STAGING_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct AddRootRequest {
    pub path: String,
    pub name: Option<String>,
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Pick "name (1).ext", "name (2).ext", ...
    Rename,
    /// Refuse with a conflict error.
    Fail,
}

/// An upload being written to a staging file before its atomic placement.
#[derive(Debug, Clone)]
pub struct UploadTarget {
    pub root: StorageRoot,
    pub parent: String,
    pub name: String,
    pub staging: PathBuf,
}

/// Owns storage-root registration and all mutating file operations.
#[derive(Clone)]
pub struct StorageManager {
    db: Database,
    device_id: String,
    /// System locations a root may neither be inside of nor contain.
    system_paths: Vec<PathBuf>,
    /// Locations a root may contain but never be inside of (NEXUS data dir).
    private_paths: Vec<PathBuf>,
}

impl StorageManager {
    pub fn new(db: Database, device_id: String, private_paths: Vec<PathBuf>) -> Self {
        let canonical = |p: PathBuf| dunce::canonicalize(&p).unwrap_or(p);
        Self {
            db,
            device_id,
            system_paths: system_paths().into_iter().map(canonical).collect(),
            private_paths: private_paths.into_iter().map(canonical).collect(),
        }
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn is_online(root: &StorageRoot) -> bool {
        root.enabled && Path::new(&root.absolute_path).is_dir()
    }

    pub fn ensure_online(&self, root: &StorageRoot) -> Result<()> {
        if Self::is_online(root) {
            Ok(())
        } else {
            Err(NexusError::Unavailable(format!("{} is offline", root.name)))
        }
    }

    pub fn resolve(&self, root: &StorageRoot, rel: &str) -> Result<PathBuf> {
        paths::resolve(Path::new(&root.absolute_path), rel)
    }

    async fn writable_root(&self, id: &str) -> Result<StorageRoot> {
        let root = self.db.get_root(id).await?;
        if root.read_only {
            return Err(NexusError::PermissionDenied(format!("{} is read-only", root.name)));
        }
        self.ensure_online(&root)?;
        Ok(root)
    }

    async fn internal_dir(&self, root: &StorageRoot, sub: &str) -> Result<PathBuf> {
        let dir = Path::new(&root.absolute_path).join(paths::INTERNAL_DIR).join(sub);
        tokio::fs::create_dir_all(&dir).await?;
        Ok(dir)
    }

    /// Registers an existing folder or drive. Files are never moved or modified.
    pub async fn add_root(&self, req: AddRootRequest) -> Result<StorageRoot> {
        let raw = req.path.trim();
        if raw.is_empty() {
            return Err(NexusError::invalid("Choose a folder or drive"));
        }
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err(NexusError::invalid("Use a full path, for example D:\\Cloud"));
        }
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|_| NexusError::NotFound(format!("Folder {raw}")))?;
        if !meta.is_dir() {
            return Err(NexusError::invalid(format!("{raw} is not a folder")));
        }
        let canonical = dunce::canonicalize(&path)?;

        for protected in &self.system_paths {
            if canonical.starts_with(protected) || protected.starts_with(&canonical) {
                return Err(NexusError::PermissionDenied(format!(
                    "{} contains or is inside the system location {}. Choose a dedicated folder instead.",
                    canonical.display(),
                    protected.display()
                )));
            }
        }
        for private in &self.private_paths {
            if canonical.starts_with(private) {
                return Err(NexusError::PermissionDenied(
                    "NEXUS's own data folder cannot be a storage root".into(),
                ));
            }
        }
        for existing in self.db.list_roots().await? {
            if existing.device_id != self.device_id {
                continue;
            }
            let other = PathBuf::from(&existing.absolute_path);
            if canonical.starts_with(&other) || other.starts_with(&canonical) {
                return Err(NexusError::Conflict(format!(
                    "This location overlaps the storage root \"{}\"",
                    existing.name
                )));
            }
        }

        let drive = drives::drive_for(&canonical);
        let root_type = if drive.as_ref().is_some_and(|d| d.removable) {
            RootType::External
        } else {
            RootType::Existing
        };
        let name = req
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .or_else(|| canonical.file_name().map(|n| n.to_string_lossy().into_owned()))
            .or_else(|| drive.as_ref().map(|d| d.label.clone()).filter(|l| !l.is_empty()))
            .unwrap_or_else(|| format!("Drive {}", canonical.display()));

        let root = StorageRoot {
            id: new_id(),
            device_id: self.device_id.clone(),
            name,
            absolute_path: canonical.to_string_lossy().into_owned(),
            root_type,
            read_only: req.read_only,
            enabled: true,
            created_at: now_ms(),
        };
        self.db.insert_root(&root).await?;
        Ok(root)
    }

    /// Removes a root's registration and index. Its files stay on disk.
    pub async fn remove_root(&self, id: &str) -> Result<StorageRoot> {
        let root = self.db.get_root(id).await?;
        self.db.delete_root(id).await?;
        Ok(root)
    }

    pub async fn create_folder(&self, root_id: &str, parent: &str, name: &str) -> Result<(StorageRoot, String)> {
        let root = self.writable_root(root_id).await?;
        let name = name.trim();
        paths::validate_name(name)?;
        let parent = paths::normalize_relative(parent)?;
        let rel = paths::join(&parent, name);
        let target = self.resolve(&root, &rel)?;
        if tokio::fs::symlink_metadata(&target).await.is_ok() {
            return Err(NexusError::Conflict(format!("\"{name}\" already exists here")));
        }
        tokio::fs::create_dir(&target).await?;
        Ok((root, rel))
    }

    /// Renames and/or moves an entry within its root. Returns the new path.
    pub async fn rename_or_move(
        &self,
        file: &FileEntry,
        new_parent: Option<&str>,
        new_name: Option<&str>,
    ) -> Result<String> {
        let root = self.writable_root(&file.root_id).await?;
        let parent = match new_parent {
            Some(p) => paths::normalize_relative(p)?,
            None => file.parent_path.clone(),
        };
        let name = match new_name {
            Some(n) => {
                let n = n.trim();
                paths::validate_name(n)?;
                n.to_string()
            }
            None => file.filename.clone(),
        };
        let new_rel = paths::join(&parent, &name);
        if new_rel == file.relative_path {
            return Ok(new_rel);
        }
        let old_lower = file.relative_path.to_lowercase();
        let parent_lower = parent.to_lowercase();
        if file.is_dir && (parent_lower == old_lower || parent_lower.starts_with(&format!("{old_lower}/"))) {
            return Err(NexusError::invalid("A folder cannot be moved into itself"));
        }

        let source = self.resolve(&root, &file.relative_path)?;
        let target = self.resolve(&root, &new_rel)?;
        let case_only = new_rel.to_lowercase() == old_lower;
        if !case_only && tokio::fs::symlink_metadata(&target).await.is_ok() {
            return Err(NexusError::Conflict(format!("\"{name}\" already exists in the destination")));
        }
        let destination_ok = match target.parent() {
            Some(p) => tokio::fs::metadata(p).await.map(|m| m.is_dir()).unwrap_or(false),
            None => false,
        };
        if !destination_ok {
            return Err(NexusError::NotFound("Destination folder".into()));
        }

        tokio::fs::rename(&source, &target).await?;
        let (extension, mime) = paths::describe(&name, file.is_dir);
        self.db
            .move_entry(&root.id, &file.relative_path, &new_rel, &parent, &name, extension, mime, file.is_dir)
            .await?;
        Ok(new_rel)
    }

    /// Moves an entry into the root's trash so it can be restored later.
    pub async fn trash(&self, file: &FileEntry, user_id: Option<&str>) -> Result<DeletedFile> {
        let root = self.writable_root(&file.root_id).await?;
        let source = self.resolve(&root, &file.relative_path)?;
        let trash_dir = self.internal_dir(&root, TRASH_DIR).await?;
        let destination = trash_dir.join(&file.id);
        if tokio::fs::symlink_metadata(&destination).await.is_ok() {
            return Err(NexusError::Conflict("This item is already in the trash".into()));
        }
        let size = if file.is_dir {
            self.db.subtree_size(&root.id, &file.relative_path).await?
        } else {
            file.size
        };
        tokio::fs::rename(&source, &destination).await?;

        let record = DeletedFile {
            id: file.id.clone(),
            root_id: root.id.clone(),
            original_path: file.relative_path.clone(),
            filename: file.filename.clone(),
            is_dir: file.is_dir,
            size,
            trash_name: file.id.clone(),
            deleted_at: now_ms(),
            deleted_by: user_id.map(str::to_string),
        };
        self.db.insert_deleted(&record).await?;
        self.db.delete_subtree(&root.id, &file.relative_path).await?;
        Ok(record)
    }

    /// Restores a trashed item to its original location (or a numbered
    /// sibling if that name is now taken). Returns the root and new path.
    pub async fn restore(&self, deleted_id: &str) -> Result<(StorageRoot, String)> {
        let record = self.db.get_deleted(deleted_id).await?;
        let root = self.writable_root(&record.root_id).await?;
        let source = Path::new(&root.absolute_path)
            .join(paths::INTERNAL_DIR)
            .join(TRASH_DIR)
            .join(&record.trash_name);
        if tokio::fs::symlink_metadata(&source).await.is_err() {
            self.db.remove_deleted(deleted_id).await?;
            return Err(NexusError::NotFound("The trashed item on disk".into()));
        }
        let (parent, name) = paths::split_parent(&record.original_path);
        tokio::fs::create_dir_all(self.resolve(&root, &parent)?).await?;

        for n in 0..1000 {
            let rel = paths::join(&parent, &paths::numbered_name(&name, n, record.is_dir));
            let target = self.resolve(&root, &rel)?;
            if tokio::fs::symlink_metadata(&target).await.is_ok() {
                continue;
            }
            tokio::fs::rename(&source, &target).await?;
            self.db.remove_deleted(deleted_id).await?;
            return Ok((root, rel));
        }
        Err(NexusError::Conflict("Could not find a free name to restore to".into()))
    }

    /// Permanently deletes a trashed item.
    pub async fn purge(&self, deleted_id: &str) -> Result<DeletedFile> {
        let record = self.db.get_deleted(deleted_id).await?;
        let root = self.writable_root(&record.root_id).await?;
        let path = Path::new(&root.absolute_path)
            .join(paths::INTERNAL_DIR)
            .join(TRASH_DIR)
            .join(&record.trash_name);
        let result = if record.is_dir {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        };
        match result {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.db.remove_deleted(deleted_id).await?;
        Ok(record)
    }

    /// Validates an upload and allocates a staging file on the same volume.
    pub async fn prepare_upload(
        &self,
        root_id: &str,
        parent: &str,
        name: &str,
        size: Option<u64>,
    ) -> Result<UploadTarget> {
        let root = self.writable_root(root_id).await?;
        let name = name.trim();
        paths::validate_name(name)?;
        let parent = paths::normalize_relative(parent)?;
        let parent_abs = self.resolve(&root, &parent)?;
        if !tokio::fs::metadata(&parent_abs).await.map(|m| m.is_dir()).unwrap_or(false) {
            return Err(NexusError::NotFound("Destination folder".into()));
        }
        if let (Some(size), Some(space)) = (size, drives::space_for(&parent_abs)) {
            if size.saturating_add(MIN_FREE_MARGIN) > space.available_bytes {
                return Err(NexusError::InsufficientStorage(format!(
                    "Not enough free space on {} ({} bytes needed, {} available)",
                    root.name, size, space.available_bytes
                )));
            }
        }
        let staging = self.internal_dir(&root, STAGING_DIR).await?.join(format!("{}.part", new_id()));
        Ok(UploadTarget { root, parent, name: name.to_string(), staging })
    }

    /// Atomically moves a finished staging file into place without ever
    /// overwriting an existing file. Returns the final relative path.
    pub async fn commit_upload(&self, target: &UploadTarget, policy: ConflictPolicy) -> Result<String> {
        for n in 0..1000 {
            let rel = paths::join(&target.parent, &paths::numbered_name(&target.name, n, false));
            let destination = self.resolve(&target.root, &rel)?;
            let staging = target.staging.clone();
            let placed = tokio::task::spawn_blocking(move || place_no_clobber(&staging, &destination))
                .await
                .map_err(NexusError::internal)?;
            match placed {
                Ok(()) => return Ok(rel),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if policy == ConflictPolicy::Fail {
                        return Err(NexusError::Conflict(format!("\"{}\" already exists", target.name)));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(NexusError::Conflict("Too many files with this name".into()))
    }

    /// Deletes abandoned staging files left by interrupted uploads.
    pub async fn clean_staging(&self, root: &StorageRoot) {
        let dir = Path::new(&root.absolute_path).join(paths::INTERNAL_DIR).join(STAGING_DIR);
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else { return };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let stale = entry
                .metadata()
                .await
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > STALE_STAGING_AGE);
            if stale {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
}

/// Places `src` at `dst` only if `dst` does not exist. Uses a hard link
/// (atomic no-clobber on NTFS) and falls back to check-then-rename on
/// filesystems without hard links (FAT32/exFAT).
fn place_no_clobber(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(src, dst) {
        Ok(()) => {
            let _ = std::fs::remove_file(src);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            if std::fs::symlink_metadata(dst).is_ok() {
                return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
            }
            std::fs::rename(src, dst)
        }
    }
}

fn system_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData"] {
        if let Some(v) = std::env::var_os(var) {
            out.push(PathBuf::from(v));
        }
    }
    #[cfg(unix)]
    for p in ["/bin", "/boot", "/dev", "/etc", "/proc", "/sys", "/usr"] {
        out.push(PathBuf::from(p));
    }
    out
}
