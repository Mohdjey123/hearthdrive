//! What a store plugin can touch while it runs: files in one storage
//! location, with only the kinds of access the owner allowed when they
//! installed it. Every file a plugin reads, adds or renames goes through
//! here; a plugin has no other way to reach the disk.

use super::Access;
use nexus_common::types::StorageRoot;
use nexus_common::{NexusError, Result};
use nexus_storage::paths;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// The most entries one listing returns.
const MAX_LIST: usize = 100_000;

/// A file or folder inside the storage location.
#[derive(Debug, Clone)]
pub struct Item {
    /// Relative to the storage location, with `/`.
    pub rel: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl Item {
    pub fn ext(&self) -> String {
        if self.is_dir {
            return String::new();
        }
        self.name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
    }

    pub fn stem(&self) -> &str {
        self.name.rsplit_once('.').map(|(s, _)| s).filter(|s| !s.is_empty()).unwrap_or(&self.name)
    }
}

pub struct Sandbox {
    granted: Vec<Access>,
    root: StorageRoot,
    deadline: Instant,
    /// Files this run added.
    pub added: Vec<String>,
    /// Files this run renamed: (old, new).
    pub renamed: Vec<(String, String)>,
}

impl Sandbox {
    pub fn new(root: StorageRoot, granted: Vec<Access>, budget: Duration) -> Self {
        Self { granted, root, deadline: Instant::now() + budget, added: Vec::new(), renamed: Vec::new() }
    }

    pub fn root(&self) -> &StorageRoot {
        &self.root
    }

    fn need(&self, access: Access) -> Result<()> {
        if !self.granted.contains(&access) {
            return Err(NexusError::PermissionDenied(format!("This plugin was not allowed to {}", access.verb())));
        }
        if access != Access::Read && self.root.read_only {
            return Err(NexusError::PermissionDenied(format!("{} is read-only", self.root.name)));
        }
        Ok(())
    }

    /// The file on disk, for reading. Refuses anything outside the storage
    /// location, HearthDrive's own folders in it, and links out of it.
    pub fn path(&self, rel: &str) -> Result<PathBuf> {
        self.need(Access::Read)?;
        paths::resolve(std::path::Path::new(&self.root.absolute_path), rel)
    }

    pub fn read(&self, rel: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.path(rel)?)?)
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.path(rel).is_ok_and(|p| p.exists())
    }

    /// What is in a folder (and its subfolders), sorted by path. Links and
    /// HearthDrive's own folders are left out.
    pub fn list(&self, folder: &str, subfolders: bool) -> Result<Vec<Item>> {
        let mut out = Vec::new();
        let mut todo = vec![paths::normalize_relative(folder)?];
        while let Some(dir) = todo.pop() {
            let disk = self.path(&dir)?;
            let Ok(entries) = std::fs::read_dir(&disk) else { continue };
            for e in entries.flatten() {
                let Ok(meta) = e.path().symlink_metadata() else { continue };
                if meta.file_type().is_symlink() {
                    continue;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                let rel = paths::join(&dir, &name);
                if paths::is_hidden_internal(&rel) {
                    continue;
                }
                if meta.is_dir() && subfolders {
                    todo.push(rel.clone());
                }
                out.push(Item { rel, name, is_dir: meta.is_dir(), size: if meta.is_dir() { 0 } else { meta.len() }, modified: meta.modified().ok() });
                if out.len() >= MAX_LIST {
                    return Err(NexusError::invalid("This folder has too many files for a plugin"));
                }
            }
        }
        out.sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
        Ok(out)
    }

    /// Adds a new file. It never replaces anything: when the name is taken,
    /// it gets a number ("name (2).jpg"). Missing folders are made.
    pub fn add(&mut self, folder: &str, name: &str, bytes: &[u8]) -> Result<String> {
        for n in 1..1000 {
            let candidate = if n == 1 { name.to_string() } else { paths::numbered_name(name, n, false) };
            if let Some(rel) = self.add_new(folder, &candidate, bytes)? {
                return Ok(rel);
            }
        }
        Err(NexusError::Conflict(format!("Too many files called {name}")))
    }

    /// Adds a new file under exactly this name, or nothing (`None`) when the
    /// name is taken.
    pub fn add_new(&mut self, folder: &str, name: &str, bytes: &[u8]) -> Result<Option<String>> {
        self.need(Access::Add)?;
        paths::validate_name(name)?;
        let folder = paths::normalize_relative(folder)?;
        let rel = paths::join(&folder, name);
        let dir = self.path(&folder)?;
        std::fs::create_dir_all(&dir)?;
        let target = self.path(&rel)?;
        let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if let Err(e) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&target);
            return Err(e.into());
        }
        self.added.push(rel.clone());
        Ok(Some(rel))
    }

    /// Renames a file in its own folder. Never replaces another file.
    pub fn rename(&mut self, rel: &str, new_name: &str) -> Result<String> {
        self.need(Access::Rename)?;
        paths::validate_name(new_name)?;
        let rel = paths::normalize_relative(rel)?;
        let (parent, _) = paths::split_parent(&rel);
        let new_rel = paths::join(&parent, new_name);
        let (from, to) = (self.path(&rel)?, self.path(&new_rel)?);
        let case_only = new_rel.to_lowercase() == rel.to_lowercase();
        if !case_only && to.symlink_metadata().is_ok() {
            return Err(NexusError::Conflict(format!("\"{new_name}\" already exists")));
        }
        std::fs::rename(&from, &to)?;
        self.renamed.push((rel, new_rel.clone()));
        Ok(new_rel)
    }

    /// The run has used its time: plugins stop and say what is left.
    pub fn out_of_time(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(dir: &std::path::Path, read_only: bool) -> StorageRoot {
        StorageRoot {
            id: "r".into(),
            device_id: "d".into(),
            name: "Disk".into(),
            absolute_path: dir.to_string_lossy().into_owned(),
            root_type: nexus_common::types::RootType::Existing,
            read_only,
            enabled: true,
            created_at: 0,
        }
    }

    #[test]
    fn only_granted_access_works() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let read_only = Sandbox::new(root(dir.path(), false), vec![Access::Read], Duration::from_secs(5));
        assert_eq!(read_only.read("a.txt").unwrap(), b"hello");
        let mut sb = read_only;
        assert!(matches!(sb.add("", "b.txt", b"x"), Err(NexusError::PermissionDenied(_))));
        assert!(matches!(sb.rename("a.txt", "c.txt"), Err(NexusError::PermissionDenied(_))));
        assert!(!dir.path().join("b.txt").exists());

        let mut none = Sandbox::new(root(dir.path(), false), vec![], Duration::from_secs(5));
        assert!(none.read("a.txt").is_err());
        assert!(none.list("", true).is_err());
        assert!(none.add("", "b.txt", b"x").is_err());
    }

    #[test]
    fn nothing_outside_the_storage_location() {
        let outer = tempfile::tempdir().unwrap();
        let inner = outer.path().join("store");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(outer.path().join("secret.txt"), "no").unwrap();
        let mut sb = Sandbox::new(root(&inner, false), vec![Access::Read, Access::Add, Access::Rename], Duration::from_secs(5));
        assert!(sb.read("../secret.txt").is_err());
        assert!(sb.add("..", "x.txt", b"x").is_err());
        assert!(sb.add("", "../x.txt", b"x").is_err());
        assert!(!outer.path().join("x.txt").exists());
        // HearthDrive's own folders are off limits too.
        assert!(sb.add(".nexus", "x.txt", b"x").is_err());
    }

    #[test]
    fn adding_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "first").unwrap();
        let mut sb = Sandbox::new(root(dir.path(), false), vec![Access::Read, Access::Add], Duration::from_secs(5));
        assert_eq!(sb.add_new("", "a.txt", b"second").unwrap(), None);
        assert_eq!(sb.add("", "a.txt", b"second").unwrap(), "a (2).txt");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "first");
        assert_eq!(sb.add("sub/deeper", "b.txt", b"x").unwrap(), "sub/deeper/b.txt");
        assert_eq!(sb.added, vec!["a (2).txt".to_string(), "sub/deeper/b.txt".to_string()]);
    }

    #[test]
    fn read_only_locations_refuse_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let mut sb = Sandbox::new(root(dir.path(), true), vec![Access::Read, Access::Add, Access::Rename], Duration::from_secs(5));
        assert!(sb.read("a.txt").is_ok());
        assert!(sb.add("", "b.txt", b"x").is_err());
        assert!(sb.rename("a.txt", "c.txt").is_err());
    }

    #[test]
    fn renames_stay_in_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.txt"), "x").unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), "y").unwrap();
        let mut sb = Sandbox::new(root(dir.path(), false), vec![Access::Read, Access::Rename], Duration::from_secs(5));
        assert!(sb.rename("sub/a.txt", "../a.txt").is_err());
        assert!(matches!(sb.rename("sub/a.txt", "b.txt"), Err(NexusError::Conflict(_))));
        assert_eq!(sb.rename("sub/a.txt", "c.txt").unwrap(), "sub/c.txt");
        assert!(dir.path().join("sub/c.txt").exists());
    }
}
