//! How the node's devices, storage locations and files map onto the folder
//! tree in File Explorer, independent of Windows.
//!
//! ```text
//! <sync root>\My Files\<device name>\<storage location name>\<path on that device>
//! <sync root>\Shared With Me\<shared folder> (<device name>)\<path inside it>
//! ```
//!
//! "My Files" holds this computer (device [`LOCAL`]) and every paired
//! device; "Shared With Me" the folders other devices shared with this one.
//!
//! Every placeholder carries an [`Identity`] (stored by Windows with the
//! placeholder), so a callback never has to work out from a display name
//! which device, location or file it is about: names only have to be valid
//! and unique on Windows.

use serde::{Deserialize, Serialize};

/// Windows keeps at most this many bytes of identity per placeholder.
pub const MAX_IDENTITY: usize = 4096;

/// The device id that stands for this computer itself.
pub const LOCAL: &str = "local";

/// Version prefix of the identity encoding.
const IDENTITY_TAG: &[u8] = b"NX1";

/// What a placeholder stands for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k")]
pub enum Identity {
    /// "My Files" (`mine`) or "Shared With Me" (`shared`).
    #[serde(rename = "s")]
    Section { s: String },
    /// A paired device's folder.
    #[serde(rename = "d")]
    Device { dev: String },
    /// A storage location on a paired device.
    #[serde(rename = "r")]
    Root { dev: String, root: String },
    /// A file or folder inside a storage location.
    #[serde(rename = "e")]
    Entry {
        dev: String,
        root: String,
        /// Path inside the storage location, `/`-separated.
        path: String,
        /// The device's index id of the file (content is fetched by it).
        id: String,
        dir: bool,
        size: u64,
        /// Last change, milliseconds since 1970.
        mtime: i64,
    },
}

impl Identity {
    /// The bytes stored with a placeholder.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let mut out = IDENTITY_TAG.to_vec();
        out.extend(serde_json::to_vec(self).ok()?);
        (out.len() <= MAX_IDENTITY).then_some(out)
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes.strip_prefix(IDENTITY_TAG)?).ok()
    }

    /// What stays the same while the item is renamed or changed: two
    /// placeholders with one key are the same item.
    pub fn key(&self) -> String {
        match self {
            Identity::Section { s } => format!("s:{s}"),
            Identity::Device { dev } => format!("d:{dev}"),
            Identity::Root { root, .. } => format!("r:{root}"),
            Identity::Entry { path, .. } => format!("e:{path}"),
        }
    }

    pub fn is_dir(&self) -> bool {
        !matches!(self, Identity::Entry { dir: false, .. })
    }

    pub fn device(&self) -> &str {
        match self {
            Identity::Device { dev } | Identity::Root { dev, .. } | Identity::Entry { dev, .. } => dev,
            Identity::Section { .. } => "",
        }
    }

    /// The storage location a placeholder is in, if any.
    pub fn root(&self) -> Option<&str> {
        match self {
            Identity::Device { .. } | Identity::Section { .. } => None,
            Identity::Root { root, .. } | Identity::Entry { root, .. } => Some(root),
        }
    }

    /// The folder to list for this placeholder's children: the path inside
    /// its storage location (`""` for the location itself).
    pub fn folder_path(&self) -> Option<&str> {
        match self {
            Identity::Device { .. } | Identity::Section { .. } => None,
            Identity::Root { .. } => Some(""),
            Identity::Entry { path, dir: true, .. } => Some(path),
            Identity::Entry { .. } => None,
        }
    }

    /// The same item at another path (renamed or moved on its device).
    pub fn moved_to(&self, new_path: &str) -> Identity {
        match self {
            Identity::Entry { dev, root, id, dir, size, mtime, .. } => Identity::Entry {
                dev: dev.clone(),
                root: root.clone(),
                path: new_path.to_string(),
                id: id.clone(),
                dir: *dir,
                size: *size,
                mtime: *mtime,
            },
            other => other.clone(),
        }
    }

    /// True when `other` describes different content for the same file.
    pub fn content_differs(&self, other: &Identity) -> bool {
        match (self, other) {
            (
                Identity::Entry { size: a, mtime: ma, id: ia, dir: false, .. },
                Identity::Entry { size: b, mtime: mb, id: ib, dir: false, .. },
            ) => a != b || ma != mb || ia != ib,
            _ => false,
        }
    }
}

/// One item a folder should contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    /// Name on Windows (see [`assign_names`]).
    pub name: String,
    pub identity: Identity,
    /// Creation time, milliseconds since 1970.
    pub created: Option<i64>,
}

impl Child {
    pub fn size(&self) -> u64 {
        match &self.identity {
            Identity::Entry { size, dir: false, .. } => *size,
            _ => 0,
        }
    }

    pub fn modified(&self) -> Option<i64> {
        match &self.identity {
            Identity::Entry { mtime, .. } if *mtime > 0 => Some(*mtime),
            _ => None,
        }
    }
}

/// Characters Windows does not allow in names, with look-alikes that it does.
fn replacement(c: char) -> Option<char> {
    Some(match c {
        '<' => '‹',
        '>' => '›',
        ':' => '꞉',
        '"' => '″',
        '/' => '∕',
        '\\' => '∖',
        '|' => '¦',
        '?' => '？',
        '*' => '∗',
        c if (c as u32) < 0x20 => '_',
        _ => return None,
    })
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
    "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Longest name Windows accepts, in UTF-16 units.
const MAX_NAME: usize = 255;

/// A name valid on Windows for something called `name` elsewhere (a phone
/// or Linux allows names Windows does not).
pub fn sanitize_name(name: &str) -> String {
    let mut out: String = name.chars().map(|c| replacement(c).unwrap_or(c)).collect();
    // Windows drops trailing dots and spaces, which would merge names.
    let kept = out.trim_end_matches(['.', ' ']).len();
    if kept < out.len() {
        let dropped = out.len() - kept;
        out.truncate(kept);
        out.extend(std::iter::repeat_n('_', dropped));
    }
    if out.is_empty() {
        out = "_".into();
    }
    let stem = out.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    truncate_utf16(&out, MAX_NAME)
}

fn truncate_utf16(s: &str, max: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        out.push(c);
    }
    out
}

/// `name (n)`, keeping the extension of a file at the end.
fn numbered(name: &str, n: usize, is_dir: bool) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if !is_dir && i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let suffix = format!(" ({n}){ext}");
    let room = MAX_NAME.saturating_sub(suffix.encode_utf16().count());
    format!("{}{suffix}", truncate_utf16(stem, room))
}

/// Gives each item a name valid and unique in one Windows folder (where
/// names differing only in case are the same). Items keep their order;
/// earlier items keep their own name, later clashes become `name (2)`.
pub fn assign_names(items: Vec<(String, Identity, Option<i64>)>) -> Vec<Child> {
    let mut taken = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for (raw, identity, created) in items {
        let base = sanitize_name(&raw);
        let mut name = base.clone();
        let mut n = 2;
        while !taken.insert(name.to_lowercase()) {
            name = numbered(&base, n, identity.is_dir());
            n += 1;
        }
        out.push(Child { name, identity, created });
    }
    out
}

/// A placeholder (or a file of the person's own) found in a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub name: String,
    /// `None` for anything that is not one of this drive's placeholders.
    pub identity: Option<Identity>,
}

/// One step to bring a folder in line with its listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The item is gone: remove the placeholder.
    Remove { name: String },
    /// Same item, new name.
    Rename { from: String, to: String },
    /// New metadata for an existing placeholder. `content` is true when the
    /// file's content changed, so a downloaded copy must be dropped.
    Update { name: String, child: Child, content: bool },
    /// A new item.
    Create { child: Child },
}

/// What has to change for a folder holding `local` to show `desired`.
/// Removals come first, then renames, updates and new items, so a name freed
/// by one step can be used by a later one. Items that are not placeholders
/// (the person's own files) are never touched, and nothing is created over
/// them.
pub fn plan(local: &[Local], desired: &[Child]) -> Vec<Change> {
    use std::collections::{HashMap, HashSet};
    let wanted: HashMap<String, &Child> = desired.iter().map(|c| (c.identity.key(), c)).collect();
    let mut removes = Vec::new();
    let mut renames = Vec::new();
    let mut updates = Vec::new();
    let mut creates = Vec::new();
    let mut present: HashSet<String> = HashSet::new();
    // Names in use after the removals: own files keep theirs.
    let mut occupied: HashSet<String> = HashSet::new();

    for item in local {
        let Some(identity) = &item.identity else {
            occupied.insert(item.name.to_lowercase());
            continue;
        };
        let key = identity.key();
        match wanted.get(&key) {
            Some(child) if child.identity.is_dir() == identity.is_dir() && present.insert(key) => {
                if child.name != item.name {
                    renames.push(Change::Rename { from: item.name.clone(), to: child.name.clone() });
                }
                if child.identity != *identity {
                    updates.push(Change::Update {
                        name: child.name.clone(),
                        child: (*child).clone(),
                        content: identity.content_differs(&child.identity),
                    });
                }
            }
            _ => removes.push(Change::Remove { name: item.name.clone() }),
        }
    }
    for child in desired {
        if !present.contains(&child.identity.key()) {
            if occupied.contains(&child.name.to_lowercase()) {
                continue;
            }
            creates.push(Change::Create { child: child.clone() });
        }
    }
    removes.into_iter().chain(renames).chain(updates).chain(creates).collect()
}

/// Splits a sync-root-relative path (`Device\Location\a\b`) into its parts.
pub fn components(relative: &str) -> Vec<&str> {
    relative.split(['\\', '/']).filter(|p| !p.is_empty()).collect()
}

/// Milliseconds since 1970 as a Windows file time (100 ns since 1601).
pub fn file_time(ms: i64) -> i64 {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    ms.saturating_mul(10_000).saturating_add(EPOCH_DIFF).max(0)
}

/// Rounds a requested byte range out to what Windows accepts in one data
/// transfer: the start down and the end up to 4 KiB, the end clamped to the
/// file's size (the last piece of a file may be shorter). Fetches at least
/// `min_len` bytes when the file has them, so reading a large file in small
/// pieces does not cost one network round trip per piece.
pub fn transfer_range(offset: u64, len: u64, file_size: u64, min_len: u64) -> (u64, u64) {
    const BLOCK: u64 = 4096;
    let start = (offset / BLOCK) * BLOCK;
    let want_end = offset.saturating_add(len.max(1)).max(start.saturating_add(min_len));
    let end = want_end.div_ceil(BLOCK).saturating_mul(BLOCK).min(file_size);
    (start.min(file_size), end.saturating_sub(start.min(file_size)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, dir: bool, size: u64, mtime: i64) -> Identity {
        Identity::Entry {
            dev: "dev1".into(),
            root: "root1".into(),
            path: path.into(),
            id: format!("id-{path}"),
            dir,
            size,
            mtime,
        }
    }

    #[test]
    fn identity_round_trips_and_is_bounded() {
        let id = entry("Photos/2024/beach.jpg", false, 1234, 1_700_000_000_000);
        let bytes = id.encode().unwrap();
        assert!(bytes.starts_with(b"NX1"));
        assert_eq!(Identity::decode(&bytes), Some(id));
        assert_eq!(Identity::decode(b"garbage"), None);
        assert_eq!(Identity::decode(b"NX1{not json"), None);
        let long = entry(&"a/".repeat(3000), false, 1, 1);
        assert!(long.encode().is_none(), "identities larger than Windows keeps are refused");
    }

    #[test]
    fn identity_describes_folders() {
        let dev = Identity::Device { dev: "d".into() };
        let root = Identity::Root { dev: "d".into(), root: "r".into() };
        assert_eq!(dev.folder_path(), None);
        assert_eq!(root.folder_path(), Some(""));
        assert_eq!(entry("a/b", true, 0, 0).folder_path(), Some("a/b"));
        assert_eq!(entry("a/b.txt", false, 0, 0).folder_path(), None);
        assert!(dev.is_dir() && root.is_dir() && !entry("x", false, 0, 0).is_dir());
        assert_eq!(root.root(), Some("r"));
        assert_eq!(dev.root(), None);
        assert_eq!(root.device(), "d");
    }

    #[test]
    fn names_are_made_valid_on_windows() {
        assert_eq!(sanitize_name("notes.txt"), "notes.txt");
        assert_eq!(sanitize_name("a:b?c*.txt"), "a꞉b？c∗.txt");
        assert_eq!(sanitize_name("back\\slash/fwd"), "back∖slash∕fwd");
        assert_eq!(sanitize_name("tab\there"), "tab_here");
        assert_eq!(sanitize_name("trailing."), "trailing_");
        assert_eq!(sanitize_name("trailing "), "trailing_");
        assert_eq!(sanitize_name(""), "_");
        assert_eq!(sanitize_name(".."), "__");
        assert_eq!(sanitize_name("a. ."), "a___");
        assert_eq!(sanitize_name("CON"), "_CON");
        assert_eq!(sanitize_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_name("console.txt"), "console.txt");
        assert_eq!(sanitize_name("Ünïcødé 名前"), "Ünïcødé 名前");
        let long = "x".repeat(300) + ".txt";
        assert_eq!(sanitize_name(&long).encode_utf16().count(), 255);
    }

    #[test]
    fn clashing_names_are_numbered() {
        let items = vec![
            ("Laptop".to_string(), Identity::Device { dev: "a".into() }, None),
            ("laptop".to_string(), Identity::Device { dev: "b".into() }, None),
            ("Laptop".to_string(), Identity::Device { dev: "c".into() }, None),
        ];
        let names: Vec<_> = assign_names(items).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["Laptop", "laptop (2)", "Laptop (3)"]);

        let files = vec![
            ("Report.pdf".to_string(), entry("Report.pdf", false, 1, 1), None),
            ("report.pdf".to_string(), entry("report.pdf", false, 1, 1), None),
            ("a:b".to_string(), entry("a:b", true, 0, 0), None),
            ("a꞉b".to_string(), entry("a꞉b", true, 0, 0), None),
        ];
        let names: Vec<_> = assign_names(files).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["Report.pdf", "report (2).pdf", "a꞉b", "a꞉b (2)"]);
    }

    fn child(name: &str, identity: Identity) -> Child {
        Child { name: name.into(), identity, created: None }
    }

    fn local(name: &str, identity: Option<Identity>) -> Local {
        Local { name: name.into(), identity }
    }

    #[test]
    fn plan_leaves_matching_folders_alone() {
        let a = entry("a.txt", false, 5, 10);
        assert!(plan(&[local("a.txt", Some(a.clone()))], &[child("a.txt", a)]).is_empty());
    }

    #[test]
    fn plan_creates_removes_and_updates() {
        let old = entry("a.txt", false, 5, 10);
        let new = entry("a.txt", false, 7, 20);
        let gone = entry("gone.txt", false, 1, 1);
        let added = entry("new", true, 0, 0);
        let changes = plan(
            &[local("a.txt", Some(old)), local("gone.txt", Some(gone)), local("mine.docx", None)],
            &[child("a.txt", new.clone()), child("new", added.clone())],
        );
        assert_eq!(
            changes,
            vec![
                Change::Remove { name: "gone.txt".into() },
                Change::Update { name: "a.txt".into(), child: child("a.txt", new), content: true },
                Change::Create { child: child("new", added) },
            ]
        );
    }

    #[test]
    fn plan_renames_devices_and_locations() {
        let dev = Identity::Device { dev: "d1".into() };
        let changes = plan(&[local("Old name", Some(dev.clone()))], &[child("New name", dev)]);
        assert_eq!(changes, vec![Change::Rename { from: "Old name".into(), to: "New name".into() }]);
    }

    #[test]
    fn plan_replaces_a_file_that_became_a_folder() {
        let file = entry("x", false, 1, 1);
        let dir = entry("x", true, 0, 0);
        let changes = plan(&[local("x", Some(file))], &[child("x", dir.clone())]);
        assert_eq!(changes, vec![Change::Remove { name: "x".into() }, Change::Create { child: child("x", dir) }]);
    }

    #[test]
    fn plan_never_touches_or_shadows_own_files() {
        let theirs = entry("notes.txt", false, 1, 1);
        let changes = plan(&[local("Notes.txt", None)], &[child("notes.txt", theirs)]);
        assert!(changes.is_empty());
    }

    #[test]
    fn plan_updates_metadata_without_dropping_content() {
        let before = entry("d", true, 0, 1);
        let after = entry("d", true, 0, 2);
        let changes = plan(&[local("d", Some(before))], &[child("d", after.clone())]);
        assert_eq!(changes, vec![Change::Update { name: "d".into(), child: child("d", after), content: false }]);
    }

    #[test]
    fn duplicate_placeholders_are_removed() {
        let a = entry("a", false, 1, 1);
        let changes = plan(&[local("a", Some(a.clone())), local("a (2)", Some(a.clone()))], &[child("a", a)]);
        assert_eq!(changes, vec![Change::Remove { name: "a (2)".into() }]);
    }

    #[test]
    fn transfer_ranges_are_block_aligned() {
        const MB: u64 = 1024 * 1024;
        // Whole small file.
        assert_eq!(transfer_range(0, 10_000, 10_000, 0), (0, 10_000));
        // Unaligned end inside the file is rounded up to a block.
        assert_eq!(transfer_range(0, 5_000, 100_000, 0), (0, 8192));
        // Unaligned start is rounded down.
        assert_eq!(transfer_range(5_000, 10, 100_000, 0), (4096, 4096));
        // Read-ahead, clamped to the file.
        assert_eq!(transfer_range(0, 4096, 100 * MB, 16 * MB), (0, 16 * MB));
        assert_eq!(transfer_range(96 * MB, 4096, 100 * MB, 16 * MB), (96 * MB, 4 * MB));
        assert_eq!(transfer_range(4096, 4096, 6000, 16 * MB), (4096, 6000 - 4096));
        // Past the end: nothing.
        assert_eq!(transfer_range(8192, 4096, 6000, 0), (6000, 0));
    }

    #[test]
    fn file_times_convert() {
        assert_eq!(file_time(0), 116_444_736_000_000_000);
        assert_eq!(file_time(1), 116_444_736_000_010_000);
    }

    #[test]
    fn paths_split_into_components() {
        assert_eq!(components("\\Laptop\\Photos\\2024"), ["Laptop", "Photos", "2024"]);
        assert_eq!(components(""), Vec::<&str>::new());
    }
}
